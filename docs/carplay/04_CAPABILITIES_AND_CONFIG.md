# Capabilities, VehicleConfig and the app-driven doctrine

> **STATUS:** CURRENT · single owner for this topic. Consolidated 2026-08-31 from pre-consolidation docs 56, 14, 22, 53; the originals are in git history and in the 2026-08-31 backup. Correct this file in place — do not add a sibling.

**Contents:** the doctrine (app owns configuration) → the YAML framework → the field glossary → the capability dossiers and their roadmap.

## Doctrine — anything configurable is app-driven

<!-- absorbed: ../carplay/04_CAPABILITIES_AND_CONFIG.md -->

**Status: OWNER DIRECTIVE — normative for all design and implementation decisions from this date.
Supersedes every prior doc where the two conflict.** This doc records the directives verbatim in
intent, defines the placement test, and indexes the corrections applied across the corpus on
2026-08-10.

---

### 1. The directives

Issued by the owner on 2026-08-10 (EV-telematics research session):

1. **The box is a dumb hardware host.** It "just happens to have" the MFi coprocessor, the WLAN/BT
   radios, and the USB OTG port (host or gadget for a wired iPhone). The rest is app-driven, as much
   as possible. This keeps the box platform-agnostic: it must not need updating when Apple changes
   CarPlay SDKs.
2. **Anything configurable regarding CarPlay is app-driven, regardless of where it lives in the
   protocol** — iAP2 configuration, vehicle information, AirPlay `/info`, SETUP. The owner's test:
   any toggle, option, or parameter **capable of multiple values** is app-driven. Named examples:
   right/left-hand drive, engine type, EV charge level, enhancedSiri support, metadata tiers.
   The box carries no defaults or opinions of its own — **it accepts what the app gives it and
   presents it to the iPhone upon connection.**
3. **Configuration is pushed at initialization.** The box stays IDLE until the app connects; the app
   pushes all config; only then does the box enable WLAN/BT or accept a wired CarPlay connection —
   so everything needed is ready before the phone ever sees the accessory.
4. **Box placement is earned, never designed-in.** The only path for functionality onto the box:
   implement app-driven first → it demonstrably FAILS under excessive testing → the owner explicitly
   approves the move. (The box-driven SETUP local response — kept as a sticky fallback after
   app-driven SETUP was implemented, measured, and validated — is the model of this pattern done
   right.)

### 2. The placement test (apply to every new feature)

The box gets new code ONLY if the feature is:
- **hardware-bound** — MFi i2c, radio bring-up, USB gadget plumbing; or
- **permanently-stable mechanics that must sit next to the hardware** — iAP2 link layer, MFi auth,
  pair-setup/pair-verify, ChaCha20 session-key derivation; or
- an **earned fallback** per directive 4 (measured app-driven failure + owner approval).

Everything else — every value, set, toggle, tier, default, geometry, format list, name, declaration
content — is app-authored, app-held, and pushed to the box, with the box as a relay/renderer. The
`CH_RTSP` app-driven SETUP relay is the proven template for "app authors, box frames."

**Corollary — single source of truth.** The app's YAML config is the single source of truth for all
capability/configuration content (this resolves a long-standing contradiction: some docs previously
named box-side `iap2-core/src/features.rs` the "single source of truth" for declarations).
`features.rs` remains the box-side table from which Identify params 6/7 and the subscribe sequence
are *generated* — the generation invariant (never hand-edit the three independently, docs/carplay/05_METADATA_AND_CONTROLS.md §5.6)
still holds — but WHICH tier/content is selected is app-driven; the compiled table and its levers
are the interim mechanism, not the design.

**Corollary — on-box levers are interim scaffolding.** `CARPLAY_WIRELESS_AUDIO`,
transport-gated defaults, and every similar env/`/tmp` control are
**current-state mechanics pending migration to the app-pushed config**, never the design. New
features must not introduce new primary on-box levers (an on-box override may exist for app-less
bench testing only, explicitly subordinate to the pushed config). A side benefit of migrating: the
"tmpfs reverts on reboot, re-arm by hand" wart disappears — the app re-pushes every session.
Already-migrated exemplars of exactly this pattern: `CARPLAY_HEVC` and `CARPLAY_CORNERMASKS` — both
armed per connection FROM the app-pushed YAML (`levers::set_hevc` / `set_cornermasks`).
`CARPLAY_CORNERMASKS`'s env form survives as a sanctioned force-arm override over the pushed config
(docs/carplay/06_AV_PIPELINE.md §4); `CARPLAY_HEVC`'s env form is now vestigial — cleared per connection by
`clear_levers`, the YAML is the only live control. **`CARPLAY_MAINBUFFERED` /
`/tmp/mainbuffered_test` joined them 2026-08-10 (workstream B4) and is the BEST exemplar of the
three**: strictly YAML-wins (no OR-force-arm — a stale bench flag must never override an app's
pushed `false`, since Phase A advertises a buffered stream the box does not serve and iOS moving
media to it silences audio), with the env surviving only on the no-config / parse-failure paths.

**Corollary — no box-side defaults for configurable values.** An unconfigured box has nothing to
fall back to; it holds IDLE until the app pushes config (directive 3). Existing compiled defaults
(metadata tier `proven`, 1920×720 resolution fallback, transport-gated audio formats, and — added
2026-08-10 by C-1 — `iap2_core::config::VehicleIdentity::baseline()`, the EngineType=Gasoline
param-20 emission) are interim safety floors to be retired as the config push covers them.

**Retirement criteria for each floor** (a floor without one becomes a permanent box-side default by
default, which is what this section exists to prevent):

| floor | retired when |
|---|---|
| metadata tier `proven` | C-9's hold-IDLE lands AND the app has shipped a tier in every push for a full soak window (B3 left this standing deliberately) |
| 1920×720 resolution | C-9 hold-IDLE soaks ≥20 sessions / 2 calendar weeks with zero sessions served off the default dims, wireless radio bring-up is app-gated, and bench workflows have moved to `CARPLAY_BENCH_CFG` |
| transport-gated audio formats | the app's "auto" mode has pushed both per-transport arms explicitly for a full soak window (B5 already demoted this to the no-config / parse-failure path) |
| `VehicleIdentity::baseline()` | with C-9's hold-IDLE — an unconfigured box should hold IDLE, not present a gasoline identity it invented. Until then the floor is what keeps a config-less or malformed-config box byte-identical to the device-proven pre-C6 Identify, which is exactly the safety property a floor is for. |

**A malformed value is not a value.** Directive 2 gives the app the choice *among valid options*; it
does not make the box a conduit for output Apple's own spec forbids. Precedent: the app-pushed
metadata path REFUSES `rx-only` (docs/carplay/05_METADATA_AND_CONTROLS.md §6.2), and C-1's identity resolver drops unknown enum names
and spec-forbidden flag combinations rather than emitting them. Refusing malformed input is not the
box deciding policy — it is the box declining to put a malformed frame on a wire where a rejection
(`0x1D03`) is unrecoverable within the session.

### 3. Known tensions the doctrine surfaces (owner decisions recorded here; each bullet marked RESOLVED or pending)

- **Accessory-initiated BT reconnect (docs/wireless/01_BT_AND_RADIO.md) — RESOLVED by owner directive, 2026-08-10 (later
  same day).** The radio policy: WiFi and BT power on ONLY on app command — never at box power-on.
  Once a radio is app-commanded on, autonomous auto-connect to known/bonded devices IS correct.
  Radios power off on app command or on loss of the app connection. Rationale: prevents a
  stale/incomplete session where the iPhone stays connected to a box with no app running. The
  shipped docs/wireless/01_BT_AND_RADIO.md page-on-boot behavior is SUPERSEDED — the power-on trigger is incorrect per the
  directive; until re-gating lands, boot-time bring-up remains today's shipped operational reality,
  not a sanctioned mode. Re-gating radio bring-up on app
  presence is an open implementation task (docs/wireless/01_BT_AND_RADIO.md's dead-end eliminations and reconnect mechanics
  stay authoritative — only the *initiation trigger* changes).
  **Implementation status (2026-08-10, workstream A):** code landed in-repo — the plan_A
  investigation found the repo boot chain already raises no radios (bring-up rides the
  host-present edge with config landed first), and closed the four real gaps: BT now POWERS OFF
  on teardown (`hciconfig hci0 down`, not just noscan), a `CT_RADIO` (0x16) CH_CTRL opcode gives
  the app a mid-session radio kill switch (flag `/tmp/radio_off`, ocbmd-owned lifecycle — box side
  complete; the app-side caller is DEFERRED: `OCBMClient.sendRadio` is currently uncalled and no
  tool emits the opcode yet, so until the Settings-toggle wiring lands the app's radio commands
  are quit/`wireless: false`-at-next-connect and the automatic off-on-app-loss), a
  supervisor startup reconciliation closes the respawn-stranding hole. The deployed-box
  script-drift audit ran 2026-08-10 (read-only, OCBM console): supervisor md5 == repo HEAD, radios
  fully down at idle, ncm_wifi/ncm_only flags absent — the deployment already exhibits app-gated
  radio behavior; only the new gap fixes remain to deploy. **DEPLOYED 2026-08-10** (ocbmd rebuilt
  armv7 + UPX-3.96 packed, supervisor script installed, prior artifacts backed up, box rebooted):
  hardware checklist test 1 (cold boot, app closed → no hci device, no wlan0, no wireless
  processes) and the supervisor-respawn reconciliation check both PASS. The remaining app/phone
  checklist steps (plan_A §5 tests 2–8 and 10) are PENDING a session with the app + iPhone.
- **Byte-pinned BT-time/wireless Identify (docs/wireless/00_WIRELESS_CARPLAY.md).** The pin is evidence-backed (device
  rejects) but the failure cause was never established, so it does not yet meet the "earned
  fallback" bar in full. It stands as a **sequencing constraint** — pushed config needs
  per-transport-arm applicability (wired vs wireless) until wireless Identify growth is
  re-validated — not as an exception to the doctrine.
- **Wireless SETUP — RESOLVED 2026-08-10 by owner directive: FLIPPED.** App-driven SETUP now runs
  on BOTH transports; the box-side gate lost its `!wireless` term. This was milestone sequencing,
  never a design preference — but it DID require one protocol change, and the first attempt was
  rejected at the gate for missing it. Two layers behave differently:
  **Streams (phase 2) — echo.** The host authors only the types it knows (video 110/111, audio
  100-102) and passes the box's own local answer through for anything else, notably the type-130 RCS
  DataStream that only wireless carries. That layer genuinely needed nothing **from the host** — the
  echo is correct and remains so. **⚠️ Corrected 2026-08-10: the BOX's own local answer for type 130
  was itself broken at the time this was written**, so "needed nothing" was true of the relay and
  false of the thing being relayed. A scid guard had been rejecting every type-130 SETUP since
  2026-07-31, so the local answer the host was faithfully passing through contained no 130 entry at
  all. See docs/carplay/05_METADATA_AND_CONTROLS.md §8. Related: the oracle compared `streams[].type` only, so a host answer that kept
  the type and dropped `streamID` would also have diffed clean — widened to a per-stream key set the
  same day (`relay.rs::setup_surface`).
  **Features (phase 1) — union, and this is the part that bit.** `authorPhase1` does not echo
  `enabledFeatures`, it AUTHORS it, and the app can only emit six tokens. On wireless the box emits
  two more — `iAPChannel` and `sessionManagement`, env-gated at the wireless spawn site with no
  config key. Overwriting would have STRIPPED `iAPChannel`, and `/info` advertises `iAPChannelInfo`
  while iOS 400s every `iAPSendMessage` unless the echo carries the token (device-observed 3/3) —
  the iAP2 tunnel's DETECT+SYN rides that command, so every wireless session would have come up
  looking healthy with a dead tunnel. Both authoring twins now PRESERVE any token outside the host's
  vocabulary, with a wireless-shaped fixture in each.
  **DEBT (workstream C/D):** the app now emits two tokens it cannot see, audit or disable. The
  doctrine-faithful end state is per-transport app config keys for them (B4's `wired:`/`wireless:`
  shape); `OPEN_FLAG_WIRELESS` already gives the host the transport it would need.
  The box's local response remains the sticky fallback on any relay failure, on both arms.

### 4. Doctrine sweep (2026-08-10)

A three-agent sweep applied the doctrine across the then-57-document corpus and the root
README/CLAUDE.md, correcting ~89 claims that asserted box ownership of something the app should own.
Its per-document verdict lists are dropped: they keyed to the pre-consolidation numbering and said
only whether a document respected the doctrine, not whether its content was true.

### 5. Implementation workstreams (the code side of the doctrine)

The 2026-08-10 gap analysis split the code work into five workstreams (A–E); B is itself staged as
B5 → B4 → B3, easiest migration first. Code comments cite these by letter (e.g. "docs/carplay/04_CAPABILITIES_AND_CONFIG.md B4"), so
this section is the anchor those citations resolve to. *(**Caveat 2026-08-16:** 16 code sites cite
numbered items — `docs/carplay/04_CAPABILITIES_AND_CONFIG.md #6`, `#25`, `#26` — and this document has **no numbered list**, so those
specific citations do not resolve to anything. The LETTER citations (B3, B5, C-2 …) do. Either number
the directives here or rewrite those 16 comments; until then, read `#N` as "the doctrine generally".)*.

| ID | Scope | Status |
|---|---|---|
| **A** | Radio lifecycle re-gating: WiFi/BT power on only on app command, off on app command or app loss (`CT_RADIO` 0x16 + `/tmp/radio_off`, `hciconfig hci0 down` in both teardown branches, supervisor startup reconciliation) | Landed + DEPLOYED 2026-08-10. Cold-boot + supervisor-respawn tests PASS on hardware; the app/phone checklist (plan_A §5 tests 2–8, 10) and the app-side `CT_RADIO` caller are pending. |
| **B5** | Audio formats: per-transport `audio.wired` / `audio.wireless` arms; the app's "auto" mode pushes both explicitly, the box presents the matching one | Landed 2026-08-10 (gate unanimous). Box transport-gated default demoted to the no-config / parse-failure floor. |
| **B4** | mainBufferedAudio: `accessoryConfig.enablesMainBufferedAudio` becomes the primary arm via `levers::mainbuffered`, read by BOTH the `/info` `mainBufferedInfo` emission and the SETUP `"mainBuffered"` echo; app default flipped OFF with a one-shot UserDefaults migration; `CARPLAY_MAINBUFFERED` / `/tmp/mainbuffered_test` demoted to the no-config / parse-failure bench fallback (strictly YAML-wins, no OR-force-arm) | Landed 2026-08-10. Wireless arm remains UNCAPTURED (docs/carplay/04_CAPABILITIES_AND_CONFIG.md) — enabling it there is a deliberate experiment that can silence media. **If that ever needs a transport-specific gate, the doctrine-faithful home is an app-side `wired:`/`wireless:` arm for the key (B5's shape) — NOT a box-side veto of a value the app pushed, which directive 2 rules out.** |
| **B3** | Metadata tier: `CARPLAY_METADATA` / `/tmp/carplay_metadata` → an app-pushed `metadata: {tier, skip}` section, armed once per process (first-arm-wins) on both the wired (iap2d) and tunnel (airplayd) arms via `iap2-core::config` | Landed 2026-08-10, NOT yet hardware-validated. App ships `proven` = the compiled floor, so the wire is unchanged until the tier is deliberately raised. Precedence pushed > env > file > compiled; `rx-only` refused on the pushed path. Raising the tier on hardware still carries the unrecoverable `0x1D03` risk — wired arm first, `idevicesyslog -p accessoryd` on every Identify-shape change. |
| **C** | Hardcoded configurable values → pushed config: Identify param 20/21 vehicle identity (the EV-telematics foundation), the advertised `/info` accessory name, display-features + HID descriptor derivation, and hold-IDLE-until-config in place of the compiled resolution default (and, with it, the retirement of the compiled metadata-tier `proven` floor B3 left standing) | **C-0 + C-1 + C-2 landed 2026-08-10; C-3..C-9 open.** C-0 (research): Apple's spec archive marks param-20 sub 2 `EngineType` and sub 11 `SupportedChargingConnectors` `[0+]`, so a hybrid emits repeated sub-2 TLVs and the planned "first entry wins" degrade path was dropped; the per-connector power subs (12-20) are count-1, which is why duplicates must be deduped. C-1 (plumbing, ZERO wire change): `iapConfig:` parsing + resolved `VehicleIdentity` in `iap2-core::config`, `build_ident_info_with` threading, param-20 emission from config, param-21 gated on a pushed `vehicleStatus:` block. The wireless (BT-time) arm substitutes the baseline INSIDE the builder, so docs/wireless/00_WIRELESS_CARPLAY.md's byte-pin is structural, not conventional. Nothing reads the identity yet — that is C-3. C-2 added the app half (a Vehicle Identity panel emitting `accessoryName:` + `iapConfig:`) and the receiver-side schema (`accessoryName` plus four `hidConfig` keys — three of which the app had ALREADY been emitting and serde had been silently discarding). Also parse-only: `apply()` never reads them and `info.rs` still emits the constant features word. **The app's param-21 control ships DISABLED behind a compile-time `vehicleStatusUnlocked = false`, which gates the EMITTER, not just the UI** — a warning shown at authoring time is the wrong shape of protection, because the setting persists and re-pushes every connection, so it would take effect by itself on the first session after C-3 with no re-consent. C-4 flips that constant in the same commit that adds the message ids. **C-6 BLOCKER — bound `accessoryName` before it reaches a `Tlv`.** Not a to-do: the reason it is urgent is that the overflow guards in `Tlv::str`/`Tlv::bytes` and `Link::build_msg` are `debug_assert!`s and are COMPILED OUT of the box's release profile, so an over-long name produces a silently truncated `0x1D01` — no panic, no log — on the one message whose rejection is unrecoverable within a session. The name lands in THREE TLV positions (param 0 `Name`, param 20 sub 1 `Name`, param 20 sub 6 `DisplayName`, plus param 21 sub 1 once armed), so per-field bounding does not generalise; the cap belongs at the iap2d call site, and the budget is a C-6 decision rather than a number guessed now. C-2 deliberately did NOT pick one — a guessed bound wearing the costume of a validated one is worse than none. **C-2 follow-up #1 — MAKE THE SWIFT EMITTER TESTABLE (highest-value open item on this workstream).** `iapConfigYAML`, `dedupedConnectors()`, `effectiveVehicleStatusCaps()` and `resetToDefault()` have ZERO automated coverage: `tests/run_tests.sh` compiles `App/VehicleConfig.swift` but not `SettingsWindow.swift`, and adding the latter drags in ControlsBridge -> AltVideoWindowController -> VideoDecoder -> VideoToolbox/IOKit, i.e. the whole app. This matters because app/box schema drift is THE repeated failure mode here — three `hidConfig` keys the app emitted were silently discarded by the box for months. The fix is to move the pure emitter into `App/VehicleConfig.swift`, which exists for precisely this reason (its own docstring: "Lives HERE, not next to the emitter in SettingsWindow.swift, because tests/run_tests.sh compiles this file and not that one"), and have the model delegate. A working proof of concept — built, and mutation-verified 9/9 by the C-2 gate, taking the Swift suite 174 -> 181 with a golden string byte-identical to `iap2-core`'s `APP_EMITTED_IAPCONFIG` so one literal is shared across Swift, iap2-core and receiver — is preserved at `scratchpad/r4_swift_emitter_testable.patch`. **The patch's golden string is STALE** — it was generated before `vehicleStatusUnlocked`, before connectors are dropped for non-electric vehicles, and before the box-side capability sort, so whoever lands this must REGENERATE the golden from the current emitter and re-diff it against `APP_EMITTED_IAPCONFIG`; the shared-literal argument only holds if the literal is re-derived, not copied. The 9/9 mutation table stands as evidence the test DESIGN works; the fixture bytes in it do not. DEFERRED FROM C-2 deliberately, not dismissed: as written it duplicates the four static vocabularies into a second type, so landing it requires rewiring `SettingsWindow` to delegate, which is a refactor that deserves its own gated change rather than riding a schema commit. C-2 is not unprotected meanwhile — the gate verified app<->box parity byte-for-byte by hand and confirmed the iap2-core fixture is byte-faithful, and `every_name_the_app_can_emit_resolves_to_a_wire_id` pins all 31 app-side names. **C-2 follow-up #2 (cosmetic, recorded so it is not lost):** the per-row connector `Picker` still offers already-used types; duplicates are dropped correctly by both the app and the box, so nothing malformed can ship, but the UI should grey used entries the way the mutually-exclusive `rangeWarning` rows are greyed — one considered pass rather than a third mechanism for the same rule. Framing stayed box-owned: param-21 capability flags are sorted onto the wire by the box, not by the app's UI grouping order. **⚠️ C-4 MUST precede C-5**: `features.rs` declares no `0xA100/0xA101/0xA102`, so emitting param 21 first would declare a component none of whose messages appear in params 6/7 — the `OptionalMsgNotValidWithoutRequiredMsgs` shape (docs/carplay/05_METADATA_AND_CONTROLS.md §5.6 rule 2), and `0x1D03` is unrecoverable. plan_C's "C-4 and C-5 are order-swappable" is retracted. |
| **D** | Schema/serde expansion: full `hidConfig` parse, `lunaConfig`/`carpConfig`/`altDisplayPanels`, folding the hand-maintained wired `SENT_MSG_IDS`/`RCV_MSG_IDS` floor into the generated table, wireless credentials from pushed config | Planned, not started. |
| **docs/carplay/04_CAPABILITIES_AND_CONFIG.md refresh** | The YAML-framework doc is THREE schema versions behind: it covers none of B5's `audio.wired`/`audio.wireless` arms, B3's `metadata:` section, or C-2's `accessoryName:` + `iapConfig:` + four new `hidConfig` keys, and needs a decision on whether it stays a schema of record now that the app's emitter is authoritative | Open. Deliberately NOT folded into B3 (it carries its own judgement calls and would have smuggled an ungated rewrite into a gated change). Drifts further with each B/C/D workstream. |
| **E** | Cleanups: the `clear_levers` gap that defeated the sanctioned app-less bench override, plus stale code comments | Landed 2026-08-10 (gate unanimous). `clear_levers` now clears cornerMasks AND logTransfer to their env presence — both sanctioned app-less overrides had been dead since they were written (docs/carplay/04_CAPABILITIES_AND_CONFIG.md had already noted the cornerMasks one "never actually worked"). `CARPLAY_HEVC` deliberately still clears to `false` (docs/carplay/04_CAPABILITIES_AND_CONFIG.md records its env form as vestigial). Five stale comments corrected. NOT yet deployed. |

---

## The YAML VehicleConfig framework

<!-- absorbed: ../carplay/04_CAPABILITIES_AND_CONFIG.md -->

The host-authoritative config pipe that replaces airplayd's hardcoded resolution (docs/carplay/06_AV_PIPELINE.md) with a
runtime `VehicleConfig` the macOS app pushes to the box. Grounded in Apple's own CarPlaySimulator
authoring schema (docs/carplay/03_SDK_GROUND_TRUTH.md §2, `reference/carplay_sdk/apple_vehicleconfigs/`). This documents the MVP
cut: the **pipe is complete end-to-end** and the **resolution lever is config-driven**; the rest of the
Apple schema parses but is not yet acted on (staged follow-ups below).

**Host-side Swift symbols named in this section predate the `VehicleConfigModel` refactor — grep
before relying on one** (`reinitializeAdapterSession` does survive). Two other figures here are
outdated: the video counter is no longer implicit (the box stamps a per-frame `seq`), and the unit
test count is 34, not 4.

### Model — host-authoritative / ephemeral
The macOS app is the source of truth. It authors an Apple-shape `VehicleConfig` YAML and ships it
**verbatim** on the OCBM `CT_SUBSCRIBE` frame (the wire already carried `[CT_SUBSCRIBE][yaml bytes]`).
The box never persists it — the config lives and dies with the session.

```
 macOS app                    ocbmd (box)                         airplayd (box)
 ─────────                    ───────────                         ──────────────
 VehicleConfig YAML  ──SUBSCRIBE──▶  write /tmp/carplay_cfg.yaml
 (AppDelegate.vehicleConfigYAML)     (atomic .tmp+rename)
 ~1 Hz HEARTBEAT ───────────▶       (keeps session alive)
                                                          iPhone control connection ▶ load_device_config()
                                                            read /tmp/carplay_cfg.yaml → VehicleConfig
                                                            → DeviceConfig → build_info() → /info
 STOP / heartbeat-loss ─────▶       remove /tmp/carplay_cfg.yaml
```

Why this shape works without re-pairing: `/info` is re-served on every control connection, and
resolution (`displays[].widthPixels/heightPixels`) is in the **reconnect-consumed class** (docs/carplay/06_AV_PIPELINE.md §"Why
no forget", docs/carplay/03_SDK_GROUND_TRUTH.md §3 Class A). A SUBSCRIBE-pushed config is picked up on the next session — no iOS
"forget", no mid-session surgery.

### Wire / storage format — the Apple YAML itself
Authoring **and** transport **and** on-box format are all the same Apple `VehicleConfig` YAML — no lossy
translation. `serde_yaml` on the box pulls only pure-Rust `unsafe-libyaml`, so it cross-compiles to
armv7-musl with no C libyaml (verified). serde **ignores unknown fields**, so the app can send the FULL
Apple template today and the box reads only what it currently consumes — forward-compatible by
construction.

### What's applied vs parse-only (this cut)
- **Applied:** `displayPanelsConfig.mainDisplayPanel.pixelDimensions.{width,height}` → `DeviceConfig.
  display_width/height` → the `/info` `displays[]`, `viewAreas`, and HID touchscreen logical maxima
  (all three via `build_info`, `info.rs`). This is the coded-resolution lever (docs/carplay/06_AV_PIPELINE.md).
- **Guard:** dimensions are applied only when both are > 0; a partial/garbled config falls through to the
  box default (never zeros the resolution out from under a session). The guard stays; the box default
  it falls to is an interim safety floor — target state per docs/carplay/04_CAPABILITIES_AND_CONFIG.md is hold-IDLE-until-config, with
  no box-side default for a configurable value.
- **Not mapped (parse-only — interim):** `lunaConfig`, `carpConfig`, `altDisplayPanels`, and most of
  `hidConfig`'s fields. These land in the schema roadmap — mapping them to the app-pushed config is
  the design (docs/carplay/04_CAPABILITIES_AND_CONFIG.md). (Several entries once on this list have since LANDED as doctrine exemplars,
  parsed and armed per connection from the pushed YAML in airplayd's `load_device_config` →
  `levers::*`: `accessoryConfig.enablesHEVC`, `videoStreamsConfig.viewAreas` + safeArea geometry,
  `altVideoStreams` (alt screen + dims), and the `hidConfig` dPad/knob/telephony support booleans.)
- **Not from YAML:** accessory identity (`device_id`, pairing `pi`) — that's pairing identity, fixed in
  airplayd to match rx_connect, never touched by the vehicle config. The `VehicleConfig.name` is template
  metadata (e.g. "Widescreen"), NOT the advertised accessory name, so it is not the field to map. The
  advertised `/info` name IS a configurable value, though — per docs/carplay/04_CAPABILITIES_AND_CONFIG.md it becomes app-pushed (a
  dedicated config field); the fixed in-binary name is interim.

### Files (direct links)
- **`crates/vendor/receiver/src/vehicle_config.rs`** (NEW; path corrected 2026-08-16 — this read
  `ncm_carplayd/receiver_core/crates/receiver/src/vehicle_config.rs`, the pre-vendoring sibling tree, see
  docs/carplay/00_ARCHITECTURE.md) — `VehicleConfig` serde
  struct mirroring Apple's schema (subset), `from_yaml()`, `apply(DeviceConfig)`. 4 unit tests: main-panel
  dims pulled (not the identically-named viewAreas `width`/`height`), non-1920 honored, partial/zero →
  base, malformed → error.
- `receiver/Cargo.toml` — added `serde` + `serde_yaml` (NOT behind the `mic-uplink` feature, so airplayd's
  `default-features=false` build includes them). `receiver/src/lib.rs` — `pub mod vehicle_config;`.
- **`ccpa/ocbmd/src/main.rs`** — `CARPLAY_CFG_FILE=/tmp/carplay_cfg.yaml`, `write_cfg_file()` (atomic,
  empty→remove) / `clear_cfg_file()`; write on SUBSCRIBE, remove on STOP + heartbeat-loss + startup.
- **`ccpa/airplayd/src/main.rs`** — `DEVICE_ID`/`PAIRING_IDENTITY` consts, `base_device_config()`,
  `load_device_config()` (reads the YAML, falls back on absence/parse error). `build_info` moved **into**
  the accept loop (was built once from the 800×480→1920×720 hardcode); built per connection now.
- **`host/CarPlayHost/carlink_macOS/App/AppDelegate.swift`** — `vehicleConfigYAML(width:height:)` builds
  the Apple-shape YAML; `client.sessionConfig` now carries it (was an ad-hoc `screen:{w,h,dpi}` string).

### Verification
- `cargo test -p receiver --no-default-features vehicle_config` → 4/4 pass.
- `cargo zigbuild --target armv7-unknown-linux-musleabihf --release -p airplayd -p ocbmd` → clean.
- Host app `xcodebuild -scheme carlink_macOS -configuration Release CODE_SIGNING_ALLOWED=NO` → BUILD SUCCEEDED.
- **Hardware validation: PASSED (2026-07-10).** Deployed at idle via `tools/uart_push.sh` (OCBM
  unavailable with the app closed), rebooted. On the app's next launch the full pipe was observed:
  - app `SUBSCRIBE sent (375 B config)` → ocbmd `SUBSCRIBE (375 B config)` → `/tmp/carplay_cfg.yaml`
    landed byte-for-byte (the `CarLink Widescreen` VehicleConfig)
  - `[airplayd] cfg: /tmp/carplay_cfg.yaml (375 B) → 1920×720` (file read + parsed + applied per connection)
  - host decoder `Format updated from SPS/PPS — 1920×720` (iPhone encoded at the YAML-driven resolution)
  - `phase=STREAMING`
  - **Controllability (different value) not yet run** — needs an app rebuild with a non-1920 `vehicleConfigYAML`
    (interrupts the live session). The `cfg:` log line already proves the file-driven path (absent → no `cfg:`).

#### Controllability + host-side geometry (2026-07-10, follow-on)
Changed the pushed resolution to **2400×960** and proved the full chain follows a live value change:
`app SUBSCRIBE → /tmp/carplay_cfg.yaml width:2400 → [airplayd] cfg … → 2400×960 → decoder Format … 2400×960`,
rendering full-frame. Then wired the host so a resolution change is one coherent action:
- **`DisplayResolution.saved` is the single source of truth** for the CarPlay resolution. `AppDelegate.
  setupDevice` builds the pushed YAML from it AND calls `windowController.applyResolution` with the same
  value, so the window shape always matches what the iPhone encodes (no letterbox from a mismatch).
  `DisplayResolution.defaultResolution` is already 2400×960.
- **Neutralized the legacy adapter-reset** in `changeResolution` + `customResolution`
  (`AppDelegate.swift`): they now resize the window only. The removed `reinitializeAdapterSession(...)`
  re-enumerated the USB adapter mid-session, which reset the host video-decrypt counter/keys while the box
  kept streaming → the video-decrypt desync. (Left `reinitializeAdapterSession` in place for the genuine
  `handleOCBMTransportLost` path.) These are interim; a broader host-app redesign will own config UI.

**Video-decrypt desync — corrected root cause.** A first 2400×960 attempt showed `video ok≈11 fail≈100+`
with box `live-A/V backpressure … dropped`. The forward-encrypted video uses an **implicit sequential
ChaCha20 counter** (`OCBMAVDecrypt.videoCounter &+= 1` per received frame; nonce = `counter_le64`, never
transmitted), so ANY lost/dropped frame desyncs decrypt permanently. BUT this was **stateful** — it did
NOT reproduce on a clean box reboot: a fresh 2400×960 session ran `video ok=190 fail=0` with **zero
backpressure drops**. So the desync came from the mid-session resolution switch / legacy menu-reset +
accumulated backpressure, not from 2400×960 being inherently over-budget. Caveat: load ≈1.33 on the single
core at 2400×960, so sustained heavy-motion video could still approach the forward-encrypt ceiling and
reintroduce drops → desync. Durable hardening (separate task): make the decrypt counter loss-tolerant
(transmit it / emit a counter-advance marker on drop) so a dropped frame skips instead of killing video.

#### Operational gotcha — app/box boot race
The host app connecting at box-uptime ≈18 s can beat the OCBM gadget being fully ready: the app's HELLO is
lost, ocbmd never handshakes, the app SUBSCRIBEs optimistically anyway (≈20 s gap in its log), and the box
stays `host_present=0` with an empty ocbmd.log. Fix = quit + reopen the app for a fresh HELLO once the box
is fully booted (no box surgery). A real fix would have the app retry HELLO until HELLO_ACK.

#### Deploy/build gotchas hit during validation (for next time)
- **xcodebuild writes to DerivedData, not `host/CarPlayHost/build/`.** The stale `build/Debug|Release`
  dirs are leftovers; launching them runs OLD code. Always launch the `-showBuildSettings`
  `BUILT_PRODUCTS_DIR` app (`~/Library/Developer/Xcode/DerivedData/carlink_macOS-…/Build/Products/<cfg>`).
- **`strings`/`nm` don't surface Swift interpolated-literal segments or (stripped) symbols** — don't use
  them to check "is my change in the binary"; verify functionally (the box shows the pushed YAML).
- With the host app closed the OCBM accessory de-enumerates → `ocbm-host push` can't reach the box; use
  `tools/uart_push.sh` for idle deploys.

### Schema coverage

The framework parses the Apple `VehicleConfig` shape; what is *applied* is listed under "What's
applied vs parse-only" above. Extending coverage is per-field work of the same shape each time:
parse the key, emit it in `/info` or the SETUP response, then act on it. The dated staging plan that
stood here is dropped — `../ops/04_OPEN_ITEMS.md` carries what is still open.

## VehicleConfig field glossary

<!-- absorbed: ../carplay/04_CAPABILITIES_AND_CONFIG.md -->

User-facing tooltip/info-popover copy for the CarPlay config UI fields that come from Apple's own
`CarPlayConfigs` schema. One entry per field:
**field** · one-line summary · explanation (≤240 chars, paste-ready for a tooltip) · evidence.

> **Coverage gap (noted 2026-08-16).** This glossary covers the APPLE schema only. The config surface the
> app has since added on top of it has **no entry here yet**: `accessoryName`, the whole `audio:` section
> (documented instead in [`../carplay/06_AV_PIPELINE.md`](../carplay/06_AV_PIPELINE.md)),
> `limitedUIConfig`, `oemIconConfig`, `appDrivenSetup`, `iapConfig`, and the `metadata: {tier, skip}`
> block (CLAUDE.md / docs/carplay/05_METADATA_AND_CONTROLS.md). Read "one entry per field" as "one entry per *Apple-schema* field", not as
> "every field in the app's UI".

Evidence tags: **[E]** grounded in a cited Apple symbol / template value / binary string; **[I]** careful
inference from the field name only (no wire mapping found) — do not treat [I] specifics as fact.

Sources: `CarPlayConfigs` Swift module in `…/CarPlaySimulator.devicekitplugin/Contents/MacOS/CarPlaySimulator`
(field names taken from the CarPlaySimulator module's public Swift symbols and the shipped
`VehicleConfigs/Configs/*.yaml` templates); project doc `../carplay/03_SDK_GROUND_TRUTH.md`.

> **⚠️ Wired vs inert — CORRECTED.** This glossary describes the *full Apple schema*: a field appearing
> here is a capability the config UI can express, **not proof the box acts on it**. Treat every entry as
> a *declaration of intent* unless its wiring is confirmed in `info.rs`/`vehicle_config.rs`. The app's
> own `SettingsWindow.inertKeys` list is only as good as its last edit — **eight of the fields it marks
> inert are armed today**; GROUND TRUTH is `vehicle_config.rs`'s accessors plus the `levers::`/`events::`
> calls in `ccpa/airplayd/src/main.rs`. The list, the eight, and the setter names:
> [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-22-1`.

---

### 1. Authoring metadata (Config level)

#### name
Human-readable label for this vehicle/accessory profile.
Just the display name of the config preset (e.g. "Standard Video Playback", "Widescreen"). It has no on-wire effect — it identifies the profile in the picker and is **NOT** sent as the accessory name (corrected 2026-08-10: `vehicle_config.rs` deliberately does not map YAML `name` onto `DeviceConfig.name` — the accessory name is the separate top-level `accessoryName:` key, parse-only until C-6). The old text claimed it.
[E] `Config.init(name: Swift.String, …)`; `name:` in every `*.yaml`; `/info` key `name` (doc 13 §3).

#### version
Config schema/format version. Always `1` today.
Marks which VehicleConfig schema this file is written against; Apple's templates all use `1`. Bump only if the field layout itself changes. Not a firmware version — that is `firmwareRevision`.
[E] `version: 1` in every `*.yaml`; `Config.init(… version: CarPlayConfigs.ConfigVersion …)`.

#### sortID
Ordering hint for the config list.
Integer that controls where this profile sorts among presets in the UI; lower shows earlier. Cosmetic only, no runtime behavior.
[E] `sortID:` in templates (e.g. Standard=2, Knob Only=1); `Config.init(… sortID: Swift.Int …)`.

---

### 2. Vehicle identity & behavior (VehicleConfig)

All from `VehicleConfig.init(manufacturer:model:hardwareRevision:firmwareRevision:serialNumber:clientOSBuildMinVersion:osInfo:rightHandDrive:electronicTollCollection:enhancedRequestCarUI:vocoderInfo:productPlanUID:removeWidgets:removeLiveActivities:)` [E] and the matching `/info` keys (doc 13 §3).

#### manufacturer
Vehicle/head-unit maker name reported to the phone.
Free-text brand string the accessory advertises in `/info` (e.g. the automaker). Cosmetic identity; iOS may use it for logging/quirk handling. Empty = unset.
[E] `VehicleConfig.manufacturer : Swift.String`; `/info` key `manufacturer`.

#### model
Head-unit model name reported to the phone.
Free-text model string sent in `/info`. Identity only; does not change capabilities. iOS surfaces it in diagnostics and may key device-specific behavior off it.
[E] `VehicleConfig.model`; `/info` key `model`; Bonjour TXT `model`.

#### hardwareRevision
Hardware revision string of the head unit.
Cosmetic identity field advertised in `/info`. Purely informational for the phone; no functional gating.
[E] `VehicleConfig.hardwareRevision`; `/info` key `hardwareRevision`.

#### firmwareRevision
Firmware/software version of the head unit.
Version string the accessory reports in `/info`. Informational; iOS may log it or apply version-specific workarounds, but it does not toggle features on its own.
[E] `VehicleConfig.firmwareRevision`; `/info` key `firmwareRevision`; TXT `srcvers` (sourceVersion).

#### serialNumber
Per-unit serial number reported to the phone.
Unique identity string for this head unit. Informational; leave blank if you don't want to advertise one.
[E] `VehicleConfig.serialNumber`; identity/`deviceID` family in `/info` (doc 13 §3).

#### clientOSBuildMinVersion
Minimum iOS build the accessory will accept.
Sets `clientOSBuildVersionMin` in `/info`. If the connected iPhone's build is older than this, CarPlay can refuse/limit the session. Leave empty to accept any build.
[E] `VehicleConfig.clientOSBuildMinVersion`; `/info` key `clientOSBuildVersionMin` (doc 13 §3).

#### osInfo
Free-text OS/platform description of the head unit.
Informational `OSInfo` string in `/info` describing the accessory's own OS/stack. No functional effect.
[E] `VehicleConfig.osInfo`; `/info` key `OSInfo`.

#### rightHandDrive
Steering side of the vehicle: on = right-hand-drive.
Tells CarPlay the driver sits on the right, so iOS would mirror driver-focused layout (e.g. control/handle placement) toward that side — if wired. **Currently inert**: it is not an `/info` key and `vehicle_config.rs` does not parse it, so the toggle rides the YAML config with no effect on the wire (corrected 2026-08-01: was described as sent in `/info` and updatable live).
[E] `VehicleConfig.rightHandDrive : Swift.Bool` (Swift/YAML field only — no `/info` key, no parser; grep of `info.rs`/`vehicle_config.rs` confirms); SettingsWindow.swift's own comment: "Not yet implemented on the box — this setting rides the config but currently has no effect on the wire."

#### electronicTollCollection
Vehicle has an ETC/toll transponder: on = advertise ETC support.
Signals the car can participate in electronic toll collection so navigation can surface toll-related guidance. Off = no ETC capability advertised.
[E] `VehicleConfig.electronicTollCollection : Swift.Bool`.

#### enhancedRequestCarUI
Enable the enhanced "request CarPlay UI" behavior.
[I] Turns on an extended flavor of the accessory→phone request-to-show-UI flow (the `requestUI`/`suggestUI` control path). On = richer/newer request-UI semantics; off = basic. Name is from the binary; exact wire delta not separately evidenced.
[I] `VehicleConfig.enhancedRequestCarUI : Swift.Bool`; related `/command` verbs `requestUI`/`suggestUI` (doc 13 §9).

#### vocoderInfo
Advertise voice-codec (vocoder) capability for phone calls.
On = the accessory publishes `vocoderInfo` so telephony/voice audio can negotiate a vocoder + sample rate. Off = omit it. Relevant for hands-free call and voice paths.
[E] `VehicleConfig.vocoderInfo : Swift.Bool`; SETUP keys `vocoderInfo`/`vocoderSampleRate`, `updateVocoderInfo` (doc 13 §7, §9).

#### productPlanUID
Apple product-plan / MFi identifier for this accessory.
Opaque identifier tying the config to a provisioned product plan. Leave as issued; it is identity/entitlement metadata, not a feature toggle.
[I] `VehicleConfig.productPlanUID : Swift.String` (name only; no wire mapping observed).

#### removeWidgets
Hide CarPlay dashboard/home widgets: on = remove them.
On = strip widget content from the CarPlay UI (useful on minimal or safety-restricted displays). Off = show the normal widget set.
[I] `VehicleConfig.removeWidgets : Swift.Bool` (name only; related to the limitedUI surface, doc 13 §3).

#### removeLiveActivities
Hide Live Activities in CarPlay: on = remove them.
On = suppress Live Activity content in the CarPlay UI. Off = allow Live Activities to appear. A UI-reduction knob, similar to removeWidgets.
[I] `VehicleConfig.removeLiveActivities : Swift.Bool` (name only).

#### nightMode
Report day/night state to CarPlay: on = night appearance.
Would drive CarPlay's light/dark UI if wired, but the `VehicleConfig.nightMode` field itself is **not** an `/info` key and is **not** parsed by `vehicle_config.rs`, so toggling it in the config has no effect on the wire (corrected 2026-08-01: was described as advertised in `/info` and switchable live via `setNightMode`). Live night-mode control DOES exist, but as a separate runtime appearance command (`setNightMode`, sent by `events.rs::send_set_night_mode`) independent of this config field.
[E] `VehicleConfig.nightMode : Swift.Bool` (Swift/YAML field only — no `/info` key, no parser; grep of `info.rs`/`vehicle_config.rs` confirms); SettingsWindow.swift's own comment: "Not yet implemented on the box"; separately, `events.rs::send_set_night_mode` sends the live `{type:"setNightMode", params:{nightMode:<bool>}}` command (`AirPlayReceiverSession.c:5278-5282`) — an appearance command, not this config field (doc 13 §9).

---

### 3. Display panel & resolution (DisplayPanelConfig)

From `DisplayPanelConfig.init(displayPanelID:pixelDimensions:physicalDimensions:initialVideoStreams:displayProperties:usesDisplayPluginDictionary:extendedModePartialPixelDimensions:)` [E].

#### displayPanelID
Identifies which physical screen this panel is.
`DisplayPanel.Main` = the primary center screen; `Alt1`/`Alt2` = additional panels (e.g. an instrument cluster). Each panel maps to one `displays[]` entry in `/info`.
[E] `displayPanelID: DisplayPanel.Main` in templates; enum `DisplayPanelID`; doc 13 §4.

#### pixelDimensions (main resolution)
Coded pixel resolution of the main display: width × height.
The exact pixel grid CarPlay renders and streams for this panel (e.g. 1920×1080, 800×480). This is the primary resolution lever — it sets `displays[].widthPixels/heightPixels` and the touch coordinate space. Changing it requires a fresh session.
[E] `pixelDimensions:{width,height}` in every template; `ConfigSize`; `/info` `widthPixels/heightPixels` (doc 13 §4, Class A).

#### alt resolution (alt panel pixelDimensions)
Pixel resolution of a secondary panel (e.g. cluster).
Same meaning as the main resolution but for an `altDisplayPanels[]`/`altVideoStreams[]` entry — typically a smaller instrument-cluster screen (e.g. 640×480). Only present on multi-display configs; omit for single-screen head units.
[E] `altDisplayPanels[].pixelDimensions` in Standard/Widescreen Instrument Cluster & Navigation templates.

#### physicalDimensions
Physical size of the display in millimeters (optional).
Real-world width/height in mm, used so iOS knows the pixel density / physical scale. `0/0` or unset = unknown, and CarPlay falls back to defaults. Cosmetic-scale hint, not a resolution.
[E] `DisplayPanelConfig.physicalDimensions: ConfigSize?`; `/info` `widthPhysical/heightPhysical` (doc 13 §4).

#### displayProperties
Special roles for this panel.
Member `showsInstruments` marks the panel as an instrument-cluster display (gauge/nav cluster) rather than a normal touch screen. Empty = a standard display.
[E] `displayProperties: [showsInstruments]` in cluster templates; enum `DisplayPanelProperty`.

#### initialVideoStreams
Which video stream(s) start on this panel.
Binds a panel to the video stream(s) shown on it at connect time (e.g. Main panel ← VideoStream.Main). Usually left implicit; used for multi-stream/cluster wiring.
[E] `DisplayPanelConfig.initialVideoStreams: [VideoStreamID]?`.

#### extendedModePartialPixelDimensions
Reduced resolution used in "extended"/partial-screen mode.
[I] Alternate pixel size for when CarPlay shares the screen (partial/extended layout) instead of owning it fully — mirrors LunaConfig's partial-screen preset. Off/unset = no partial mode.
[I] `DisplayPanelConfig.extendedModePartialPixelDimensions: ConfigSize?`; cf. Luna `Partial Screen`/`extendedMode` (Standard Video Playback template).

---

### 4. Video stream (VideoStreamConfig)

From `VideoStreamConfig.init(videoStreamID:viewAreas:framesPerSecond:physicalDimensions:)` [E]; per-stream `hidConfig` and `primaryInput` live alongside it (getters `HIDConfig` / `PrimaryInput`) [E].

#### videoStreamID
Identifies which video stream this is.
`VideoStream.Main` = the primary UI stream; `VideoStream.Alt1` = a secondary stream (e.g. cluster map). Each becomes an AirPlay screen stream.
[E] `videoStreamID: VideoStream.Main` in templates; enum `VideoStreamID`.

#### maxFPS (framesPerSecond)
Maximum frame rate CarPlay streams to this display.
Caps the video refresh rate (Apple uses 60). Higher = smoother animation but more decode/bandwidth; lower can ease a constrained pipeline. Advertised as `displays[].maxFPS`; part of the reconnect-only (Class A) set.
[E] `VideoStreamConfig.framesPerSecond: FramesPerSecond?`; `/info` key `maxFPS` (CarPlaySDK string; doc 13 §4).

#### primaryInput
The main input device the driver uses on this screen.
`Touchpad` = a remote touchpad drives focus/selection; `Knobs` = a rotary controller does. Tells CarPlay which control model to optimize the UI for (focus ring vs. direct touch). Maps to `displays[].primaryInputDevice`.
[E] `primaryInput: Touchpad`/`Knobs` in templates; `PrimaryInput.init(airPlayValue:)`/`(rawValue:)`; doc 13 §4.

---

### 5. View areas (ViewAreaConfig)

From `ViewAreaConfig.init(viewArea:safeArea:safeAreaDisabled:statusBarEdge:transitionControl:focusTransfer:drawUIOutsideSafeArea:)` [E]. A stream can list multiple view areas (e.g. full vs. split layouts).

#### viewArea
The rectangle of the screen CarPlay may draw into.
`originX/originY/width/height` (pixels) defining the drawable region for this layout. Full-screen configs set it to the whole panel; split/partial layouts use a sub-rect. Multiple entries = selectable layouts.
[E] `viewArea:{originX,originY,width,height}` in every template; `ConfigRect`; `/info` `viewAreas[]` (doc 13 §4).

#### safeArea
Inner rectangle guaranteed free of obstructions.
The region inside the view area where important UI won't be clipped by bezels/rounded corners. iOS keeps controls within it. Usually equals the view area unless the display has cutouts.
[E] `safeArea:{…}` in templates; `/info` `safeArea{originXPixels…}` (doc 13 §4).

#### safeAreaDisabled
Ignore the safe area for this view: on = draw edge-to-edge.
On = tell CarPlay there is no inset region and it may use the whole view area. Off = honor the declared safe area. Use only on truly rectangular, obstruction-free screens.
[E] `ViewAreaConfig.safeAreaDisabled: Swift.Bool?`.

#### drawUIOutsideSafeArea
Allow UI to extend past the safe area: on = permit overflow.
On = CarPlay may render some UI into the region between the safe area and the view-area edge (background/immersive content). Off = keep all UI inside the safe area.
[E] `ViewAreaConfig.drawUIOutsideSafeArea: Swift.Bool?`; `/info` `safeArea.drawUIOutsideSafeArea` (doc 13 §4).

#### statusBarEdge
Which screen edge the CarPlay status bar hugs.
Places the status/clock strip along a chosen edge (top/leading/…) for this layout. Cosmetic layout hint per view area.
[E] `ViewAreaConfig.statusBarEdge: StatusBarEdge?`; `/info` `viewAreaStatusBarEdge` (doc 13 §4).

#### transitionControl
Let the accessory animate view-area transitions: on = enabled.
[I] On = the head unit participates in animating changes between view areas (e.g. resize/split transitions) rather than a hard cut. Off = no transition handling.
[I] `ViewAreaConfig.transitionControl: Swift.Bool?`; `/info` `viewAreaTransitionControl` (doc 13 §4).

#### focusTransfer (view area)
Allow keyboard/knob focus to move into/out of this view area.
[I] On = focus can hand off between this area and adjacent UI (multi-region focus). Off = focus stays put. Per-view-area cousin of the accessory-wide focusTransfer feature.
[I] `ViewAreaConfig.focusTransfer: Swift.Bool?`; cf. `enablesFocusTransfer` (doc 13 §3).

---

### 6. HID / input capabilities (hidConfig)

From `HIDConfig.init(knobSupport:knobSupportsHomeAndBackButton:knobSupportsNudge:knobSupportsDPadNudgeFudge:knobFocusTransferLeft:Right:Up:Down:lockPTFocus:touchScreenMode:touchScreenSupportsCancel:touchScreenSupportsMultiTouch:steeringWheelSupport:telephonyButtonsSupport:mediaButtonsSupport:touchpadSupport:touchpadWidth:touchpadHeight:touchpadButtonsSupport:dPadSupport:notificationButton:)` [E]. In Apple's schema each `true` publishes the matching descriptor. **ON THIS BOX, FOUR KEYS CHANGE AN EMITTED DESCRIPTOR** (corrected 2026-08-10; count raised from three 2026-08-16). Three ADD a device: `dPadSupport` (uid 3), `knobSupport` (uid 4), `telephonyButtonsSupport` (uid 5). A fourth SWAPS one: `touchScreenSupportsMultiTouch` replaces the uid-1 single-contact descriptor with Apple's **133-byte** two-`Finger` multi-touch descriptor (`info.rs` `touchscreen_multi_descriptor`, armed per connection by `events::set_multi_touch_advertised(vc.multi_touch_support())`). The touchscreen (uid 1) and media buttons (uid 2) are emitted UNCONDITIONALLY regardless of `touchScreenMode`/`mediaButtonsSupport`, and `touchpadSupport`/`steeringWheelSupport` publish NO device at all — they are C-7/C-8 inputs to the `displays[].features` word (doc 13 §8).

#### knobSupport
Head unit has a rotary control knob: on = expose a knob HID.
On = advertise a rotary knob (turn = scroll/rotate, press = select) so CarPlay drives a focus-based UI. Off = no knob. Prerequisite for the knob sub-options below.
[E] `HIDConfig.knobSupport`; `HIDKnobCreateDescriptor` (doc 13 §8).

#### knobSupportsHomeAndBackButton
Knob assembly has Home and Back buttons: on = expose them.
On = the knob reports dedicated Home and Back presses. Off = those buttons aren't advertised. Requires knobSupport.
[E] `HIDConfig.knobSupportsHomeAndBackButton`; templates set it with `knobSupport`.

#### knobSupportsNudge
Knob can nudge (tilt) in directions: on = enable nudge.
On = the knob reports directional nudges/tilts (left/right/up/down) in addition to rotation, used for grid navigation. Off = rotation + press only.
[E] `HIDConfig.knobSupportsNudge`; set alongside knob in templates.

#### knobSupportsDPadNudgeFudge
Treat knob nudges as D-pad steps (tolerance/"fudge"): on = enable.
[I] On = enables a compatibility mapping that interprets imprecise knob nudges as clean D-pad directions. Off = raw nudge handling. Fine-tuning flag for knob feel.
[I] `HIDConfig.knobSupportsDPadNudgeFudge` (name only).

#### knobFocusTransferLeft / Right / Up / Down
Knob can push UI focus off-screen in that direction: on = enable per side.
Each toggle lets a knob nudge at the edge hand focus to an adjacent surface/display in that direction (left/right/up/down). Off = focus stops at the edge. Used for multi-zone cockpits.
[E] `HIDConfig.knobFocusTransfer{Left,Right,Up,Down}`; strings `_knobFocusTransfer*`; cf. focusTransfer (doc 13 §3).

#### lockPTFocus
Lock passenger/pass-through focus: on = prevent focus leaving.
[I] On = pin focus so it can't transfer away (e.g. lock to the driver zone). Off = normal focus transfer. Counterpart to the knobFocusTransfer flags.
[I] `HIDConfig.lockPTFocus` (name only).

#### touchScreenMode
Type of touchscreen the display exposes. Enum — see values below.
Selects whether the panel reports a high-fidelity, low-fidelity, or no touchscreen HID. Governs how precisely iOS receives touches and which touch descriptor is published.
[E] `HIDConfig.touchScreenMode: TouchScreenMode`; `TouchScreenMode.init(rawValue:)`; `touchScreenMode:` in templates.

##### touchScreenMode = "High Fidelty" [sic]
Full-precision absolute touchscreen: drags, swipes, gestures.
Publishes a high-fidelity touch HID that streams continuous absolute coordinates, so scrolling, dragging and gestures work like a phone. Maps to the HighFidelityTouch display feature (bit 0x08, corrected 2026-08-01: was 0x02). Use for real capacitive screens. (Apple's literal spelling is "High Fidelty".)
[E] `touchScreenMode: High Fidelty` in most templates; enum value `highFidelity`; display features 0x0A=HighFidelityTouch|Knobs (doc 13 §4).

##### touchScreenMode = "Low Fidelty" [sic]
Coarse touchscreen: taps/discrete points only, no smooth gestures.
Publishes a low-fidelity touch HID — good for resistive or imprecise panels. iOS gets tap/selection points rather than fluid drag streams, so it adapts the UI to discrete input.
[E] enum value `lowFidelity`; `TouchScreenMode` raw string `Low Fidelty`.

##### touchScreenMode = "Disabled"
No touchscreen at all.
No touch HID is published; the driver must use the knob/touchpad/D-pad instead. Used by knob-only head units (e.g. the "Standard Knob Only" template).
[E] `touchScreenMode: Disabled` in Standard Knob Only template; enum value `disabled`.

#### touchScreenSupportsCancel
Touchscreen can report a canceled touch: on = enable.
On = the touch HID includes a cancel flag so an interrupted/palm-rejected touch is reported as canceled rather than a false tap. Off = no cancel signal. Uses the "WithCancel" descriptor variant.
[E] `HIDConfig.touchScreenSupportsCancel`; `HIDTouchScreen…WithCancel…CreateDescriptor` (doc 13 §8).

#### touchScreenSupportsMultiTouch
Touchscreen supports multi-finger input: on = enable.
On = publish a multi-touch descriptor (12-byte, two-contact reports) so pinch/two-finger gestures work. Off = single-touch only. Requires a real multitouch panel.
[E] `HIDConfig.touchScreenSupportsMultiTouch`; multi-touch 12B report (doc 13 §8).

#### steeringWheelSupport
Steering-wheel control buttons: on = expose a steering-wheel HID.
On = advertise wheel-mounted buttons (e.g. next/prev, voice, phone) so the driver can control CarPlay from the wheel. Off = none.
[E] `HIDConfig.steeringWheelSupport`; `HIDSteeringWheelCreateDescriptor` (doc 13 §8).

#### telephonyButtonsSupport
Physical call buttons (accept/end): on = expose them.
On = advertise dedicated telephony buttons so answering/ending calls from hardware works. Off = no hardware call keys. Enabled in nearly every template.
[E] `HIDConfig.telephonyButtonsSupport`; `HIDTelephonyCreateDescriptor` (doc 13 §8).

#### mediaButtonsSupport
Physical media buttons (play/pause, skip): on = expose them.
On = advertise transport/media keys so hardware buttons control playback. Off = none. Standard on almost all head units.
[E] `HIDConfig.mediaButtonsSupport`; `HIDMediaButtonsCreateDescriptor` (doc 13 §8).

#### touchpadSupport
Head unit has a touchpad: on = expose a touchpad HID.
On = advertise an absolute touchpad (finger position drives a focus cursor), common on knob/touchpad cars. Off = no touchpad. Often paired with `primaryInput: Touchpad`.
[E] `HIDConfig.touchpadSupport`; `HIDTouchpad…CreateDescriptor` (doc 13 §8).

#### touchpadWidth
Physical touchpad width (used to scale touchpad coordinates).
Sets the touchpad's logical/physical width so absolute finger positions map correctly. Only meaningful when touchpadSupport is on. Unset = descriptor default.
[E] `HIDConfig.touchpadWidth: Swift.Int?`; touchpad abs coords in µm (doc 13 §8).

#### touchpadHeight
Physical touchpad height (used to scale touchpad coordinates).
Companion to touchpadWidth — the touchpad's height for correct coordinate scaling. Only used when touchpadSupport is on.
[E] `HIDConfig.touchpadHeight: Swift.Int?`.

#### touchpadButtonsSupport
Touchpad has clickable buttons: on = expose them.
On = the touchpad reports press/click buttons in addition to finger position. Off = position only. Requires touchpadSupport.
[E] `HIDConfig.touchpadButtonsSupport`; `HIDTouchpadButtonsCreateDescriptor` (doc 13 §8).

#### dPadSupport
Directional pad: on = expose a D-pad HID.
On = advertise a 4/8-way directional pad for grid navigation. Off = none. Present on most touchpad-equipped templates; absent on knob-only.
[E] `HIDConfig.dPadSupport`; `HIDDPadCreateDescriptor`; 2B dpad report (doc 13 §8).

#### notificationButton
Dedicated notifications button: on = expose it.
[I] On = advertise a hardware button that opens/toggles CarPlay notifications. Off = none.
[I] `HIDConfig.notificationButton` (name only).

---

### 7. Accessory feature toggles (accessoryConfig)

From `AccessoryConfig.init(enablesUIContext:lastOnDisplayURLs:nowOnDisplayURLs:enablesEnhancedSiri:enablesCornerMasks:enablesFocusTransfer:enablesHEVC:enablesMainBufferedAudio:enablesVideoPlayback:enablesVehicleDataProtocol:enablesFileTransfer:enablesLogTransfer:enablesUISync:enablesDCX:enablesUIAppearance:enablesMapAppearance:approvedClusterURLs:sessionManagement:legacyDisplayInfo:clusterAccessoryContent:initialIconAppearance:initialIconStyle:)` [E]. Most `enables*` map to an AirPlay feature that is advertised in `/info` and must survive the SETUP feature-intersection gate (doc 13 §3).

#### enablesUIContext
Report what CarPlay app is on-screen: on = enable UI context.
On = the accessory publishes/updates the "UI context" (which app/screen is showing, via last/now-on-display URLs), letting the car react to CarPlay state. Off = no context reporting.
[E] `enablesUIContext`; `_enablesUIContext`; log `AirPlay supportsUIContext`; feature `uiContext` (SDK); doc 13 §3.

#### lastOnDisplayURLs
Seed URLs describing what was previously on screen.
The initial "last on display" UI-context values sent with `uiContext` (e.g. app URLs). Only meaningful when enablesUIContext is on; usually left empty and updated at runtime.
[E] `AccessoryConfig.lastOnDisplayURLs: [Swift.String]?`; `/info` `uiContextLastOnDisplayURLs` (SDK string).

#### nowOnDisplayURLs
Seed URLs describing what is currently on screen.
The initial "now on display" UI-context values for `uiContext`. Paired with enablesUIContext; typically empty at config time and updated live as apps change.
[E] `AccessoryConfig.nowOnDisplayURLs: [Swift.String]?`; `/info` `uiContextNowOnDisplayURLs` (SDK string).

#### enablesEnhancedSiri
Enhanced Siri integration: on = advertise it.
On = the accessory publishes `enhancedSiriInfo` (button trigger, supported languages, mixable audio) so the car can invoke and mix Siri richly. Off = basic/none. Must also pass SETUP.

**⚠️ INERT ON THIS BOX — corrected 2026-08-10.** The app EMITS `enablesEnhancedSiri` (`SettingsWindow.swift`) but the box has **NO PARSER** for it: the key is serde-ignored, and `enhancedSiriInfo` appears nowhere in `crates/` or `ccpa/`. Enhanced Siri (`AuxIn` 107 / `AuxOut` 106) is unimplemented — `session.rs`'s SETUP phase-2 default arm omits AuxOutAudio / AuxInAudio / MainBuffered from the response (the "Still unimplemented and therefore omitted" comment, ~`session.rs:1516`; anchor corrected 2026-08-16, was `session.rs:1402`). Note BUTTON-Siri does work (mic uplink over `CH_MIC`, `requestSiri`); it is the always-on path that does not exist.
[E] `enablesEnhancedSiri`; log `AirPlay supportsEnhancedSiri`; feature `enhancedSiri`/`enhancedSiriInfo` (SDK); doc 13 §3,§7.

#### enablesCornerMasks
Non-rectangular screen corner masks: on = advertise support.
On = tell CarPlay the display can be masked at the corners (rounded/cut edges) via `cornerMasks`; the car then streams per-corner opaque bitmaps at runtime. Off = assume a plain rectangle. This is the CarPlay "cutout" mechanism (no notch/radius keys).
[E] `enablesCornerMasks`; log `AirPlay supportsCornerMasks`; feature `cornerMasks` (SDK); doc 13 §4.

#### enablesFocusTransfer
Hand UI focus between CarPlay and native car UI: on = enable.
On = advertise `focusTransfer` so focus can move between CarPlay and the head unit's own UI (e.g. split screens / multiple displays). Off = CarPlay keeps focus to itself.
[E] `enablesFocusTransfer`; log `AirPlay supportsFocusTransfer`; feature `focusTransfer` (SDK); doc 13 §3.

#### enablesHEVC
Accept H.265/HEVC video: on = advertise HEVC.
On = the accessory publishes a non-null `hevcInfo`, allowing iOS to stream more efficient HEVC (`hvc1`) instead of only H.264. Off = H.264 only. Requires the head unit to actually decode/forward HEVC; `hevc` is separate from `h.264Level5.1`.
[E] `enablesHEVC`; `AccessoryConfig.enablesHEVC(supports:)->Bool`; feature `hevc`/`hevcInfo` (SDK); doc 13 §5.

#### enablesMainBufferedAudio
Buffered (media/music) audio stream: on = enable.
On = advertise `mainBufferedInfo` so iOS uses the buffered TCP audio path for music/media. Off = no buffered path. Enabled in every template. **CORRECTED 2026-08-02: this previously said "better quality than realtime" — it is not a quality change.** The codec is unchanged (same `audioFormat` bitmask; there is no higher-fidelity entry to select). What improves is delivery integrity — the head unit holds up to a **2-minute** buffer fed faster than real time, so playback survives an intermittent link loss. Apple frames it as responsiveness + dropout survival, never quality: `wwdc2023-10150.txt:136-142` (see `docs/ops/03_REFERENCE_INDEX.md` §E). Full evidence incl. the receiver-side Simulator symbols: `docs/carplay/06_AV_PIPELINE.md`.
[E] `enablesMainBufferedAudio`; log `AirPlay supportsMainBufferedAudio`; feature `mainBuffered`/`mainBufferedInfo`; doc 13 §7.

#### enablesVideoPlayback
Full-screen video app playback: on = allow video content.
On = the accessory advertises `allowVideoPlayback`/`videoPlayback`, letting iOS stream arbitrary fullscreen video (e.g. media/streaming apps, using the Luna preset sizes) — not just the CarPlay UI. Off = UI/nav rendering only, no free-form video surface. Set in the "Standard Video Playback" template.
[E] `enablesVideoPlayback`; strings `allowVideoPlayback`,`videoPlayback`; `enablesVideoPlayback: true` in Standard Video Playback template + LunaConfig presets.

#### enablesVehicleDataProtocol
Vehicle Data Protocol (nav telemetry / route data): on = enable.
On = advertise `vehicleStateProtocol`, opening the two-channel VehicleDataProtocol used for navigation/route status and vehicle-state exchange (VDC accessory). Off = no vehicle-data channel. Needed for turn-by-turn cluster/nav integration.
[E] `enablesVehicleDataProtocol`; feature `vehicleStateProtocol`/`vehicleStateProtocolInfo` (SDK); doc 13 §3,§6.

#### enablesFileTransfer
Asset/file transfer channel: on = enable.
[I] On = advertise the file/asset transfer capability so iOS can push assets to the accessory. Off = disabled. (Related SDK string `assetTransferFailed`; exact feature token not separately logged.)
[I] `enablesFileTransfer`; `AccessoryConfig.enablesFileTransfer(supports:)->Bool`; doc 13 §3.

#### enablesLogTransfer
Diagnostic log transfer: on = allow log pull.
On = advertise `logTransfer`/`logTransferInfo` so iOS can request diagnostic log archives from the accessory (`handleLogArchiveRequest`). Off = no log upload. For diagnostics only.
[E] `enablesLogTransfer`; log `AirPlay supportsLogTransfer`; feature `logTransfer`/`logTransferInfo` (SDK); doc 13 §3,§9.

#### enablesUISync
Synchronize UI state between phone and car: on = enable.
[I] On = advertise UI-sync so CarPlay and the head unit keep certain UI state in step. Off = no sync. Name from the binary; no distinct wire feature string observed beyond `enablesUISync`.
[I] `enablesUISync`; `_enablesUISync` string; doc 13 §3.

#### enablesDCX
"DCX" capability. Purpose not evidenced — leave default unless directed.
[I] Only the property name `enablesDCX`/`_enablesDCX` exists in the simulator; there is NO corresponding string in the CarPlaySDK and no on-wire feature mapping was found. The expansion (plausibly a "dynamic content" exchange) is unverified — do not rely on a specific meaning.
[I] `AccessoryConfig.enablesDCX: Swift.Bool?` (sim only; absent from CarPlaySDK; no wire evidence).

#### enablesUIAppearance
Let the car theme CarPlay's UI appearance: on = enable.
On = advertise UI-appearance control so the head unit can drive CarPlay's look (e.g. light/dark/accent via `uiAppearanceUpdate`). Off = CarPlay uses defaults. Enabled in every template.
[E] `enablesUIAppearance`; `enablesUIAppearance: true` in all templates; `/command uiAppearanceUpdate` (doc 13 §9).

#### enablesMapAppearance
Let the car theme the CarPlay map: on = enable.
On = advertise map-appearance control so the head unit can influence Maps styling/zoom (`mapAppearanceUpdate`, `changeMapZoomLevel`), and it is required for the alt/cluster map stream. Off = default map styling.
[E] `enablesMapAppearance`; set in all templates incl. cluster/nav; `/command mapAppearanceUpdate` (doc 13 §6,§9).

#### approvedClusterURLs
Allow-list of cluster "suggest UI" URLs.
Set of URLs the accessory will accept for the instrument-cluster suggest-UI flow (e.g. `maps:/car/instrumentcluster/map`). Restricts what content can be pushed to the cluster. Empty = none approved.
[E] `AccessoryConfig.approvedClusterURLs: Set<ClusterSuggestUIURL>?`; strings `approvedClusterURLs`,`ClusterSuggestUIURL`; doc 13 §6.

#### sessionManagement
Advertise session-management capability: on = enable.
On = publish `sessionManagementInfo` so start/stop/keep-alive session control is negotiated. Off = omit. Part of the SETUP-gated feature set.
[E] `sessionManagement`; log `AirPlay supportsSessionManagement`; feature `sessionManagement`/`sessionManagementInfo` (SDK); doc 13 §3.

#### legacyDisplayInfo
Emit the older display-info format: on = legacy mode.
[I] On = advertise displays using the legacy `/info` layout for backward compatibility with older iOS; off = modern format. Compatibility shim; leave off unless targeting old clients.
[I] `AccessoryConfig.legacyDisplayInfo: Swift.Bool?`; string `legacyDisplayInfo`.

#### clusterAccessoryContent
Content identifier/payload for the instrument cluster.
[I] A string naming the accessory-provided content shown in the cluster (paired with approvedClusterURLs / cluster map stream). Empty = none. Content plumbing for cluster displays.
[I] `AccessoryConfig.clusterAccessoryContent: Swift.String?`; strings `clusterAccessoryContent`, `clearContents`; doc 13 §6.

#### initialIconAppearance
Initial appearance mode of the CarPlay app icon.
[I] Integer selecting the icon's starting appearance variant shown to the driver at connect. Cosmetic; leave at default unless the head unit ships specific icon art.
[I] `AccessoryConfig.initialIconAppearance: Swift.Int?`; strings `initialIconAppearance`; cf. OEMIconConfig (doc 13 §3).

#### initialIconStyle
Initial style of the CarPlay app icon.
[I] Integer choosing the icon's starting style variant. Cosmetic companion to initialIconAppearance; default is fine for most units.
[I] `AccessoryConfig.initialIconStyle: Swift.Int?`; strings `initialIconStyle`.

---

### Notes on fidelity
- `High Fidelty` / `Low Fidelty` are Apple's own misspellings in the YAML/enum — keep them verbatim in configs.
- Fields marked **[I]** carry only a name from the compiled binary; the tooltip states a plausible meaning but
  should not be presented as authoritative. `enablesDCX` in particular has **no** corroborating evidence anywhere
  in the CarPlaySDK.
- `enables*` toggles advertise a capability in `/info`; the phone must also keep it through the SETUP
  feature-intersection gate for it to actually take effect (doc 13 §3).

---

### ADDENDUM 2026-07-12 — Safe / View Areas (curved-panel UI inset) — IMPLEMENTED

**What it is:** per video stream, CarPlay declares a **viewArea** (where content maps — we keep it the
full coded frame, so video always fills the rectangular panel) and a **safeArea** (an inset rectangle
inside it where CarPlay keeps its interactive UI). Used for curved/irregular displays: the panel is a
rectangle, the safe area excludes the occluded edges. Capture-verified shape (a real head unit's 1920
cluster): `safeArea originXPixels:100, widthPixels:1720` = 100px inset each side.

**Model = absolute rectangle, not a single offset.** Wire keys `originXPixels/originYPixels/
widthPixels/heightPixels` (flat viewArea entry, `safeArea{…}` with `drawUIOutsideSafeArea` as a
sibling key at the same indent, corrected 2026-08-01: was described as nested inside `safeArea`). Rule:
`safeArea ⊆ viewArea ⊆ resolution`. Coordinate origin top-left. Declared statically per display in
`/info` `displays[].viewAreas[]`; iOS honors the inset ONLY when `viewAreas` is echoed in the SETUP
`enabledFeatures` (there's also a runtime `ViewAreaUpdate` that just switches the active index — not
needed for the basic behavior).

**End-to-end implementation (both main + cluster):**
- **UI** (`SettingsWindow.swift` `SafeAreaField`): per-edge insets (L/T/R/B px, 0 = flush) for Main and
  Cluster video, a live "safe box" readout (red if it collapses), and an "Allow UI outside safe area"
  toggle. Insets → absolute rect on Save.
- **YAML** (`va()`): emits `viewArea` (full frame) + `safeArea{originX/Y/width/height}` per stream,
  with `drawUIOutsideSafeArea` as a sibling key at the same indent as `safeArea:`, not nested inside it
  (corrected 2026-08-01: was described as nested inside `safeArea{…}`).
- **Parser** (`vehicle_config.rs`): parses `viewAreas[]` (`ViewAreaEntry`/`AreaRect`/`SafeRect`) onto
  `DeviceConfig.{main,alt}_safe_area`. A **full-frame** safeArea is treated as full-bleed (`None`) so
  non-curved configs stay byte-identical; only a real inset flips `view_areas_enabled()`.
- **`/info`** (`info.rs` `view_areas`): emits the inset safeArea per display, validated `⊆ panel`
  (fallback full-bleed). `airplayd` sets `CARPLAY_VIEWAREAS` when a real inset/toggle is present →
  `session.rs` echoes `"viewAreas"` in `enabledFeatures`.

**VALIDATED 2026-07-12 (hardware):** a 100px L/R inset on the 1920×720 main → `cfg: … viewAreas=true
safe=Some((100, 0, 1720, 720))`, RECORD reached, video `fail=0`, **0 teardowns** — iOS accepts the
inset `/info` + `viewAreas` echo cleanly. Full-bleed default = `viewAreas=false safe=None` (byte-
identical to before). Unit tests: `parses_inset_safe_area_onto_device_config`,
`full_frame_safe_area_is_not_an_inset`, `explicit_enables_view_areas_flag_wins` (+ existing), all green.

---

## Capability dossiers and roadmap

<!-- absorbed: ../carplay/04_CAPABILITIES_AND_CONFIG.md -->

**Status: RESEARCH pass — no code changed.** This document inventories ten CarPlay capabilities that
the macOS host app already exposes as toggles but the CCPA box mostly does not implement, pins each
one's on-the-wire mechanism against the licensed/extracted sources, and ranks all ten for
implementation. It is the deliverable that drives the implementation work; the wire details, message
ids, `/info` keys and citations below are transcribed verbatim from the capability dossiers and are
meant to be acted on directly. Nothing here has been built or device-proven yet — where a dossier
grades a capability "testable now" vs "blocked-untestable", that is a prediction about what the
existing hardware can prove, not a result.

---

### Capability status

Ten CarPlay capabilities the macOS app exposes as toggles. Each row is the current state of the box
code, not a plan. Research detail (iOS-27 symbol evidence, disassembly chains, WWDC provenance) was
cut on 2026-08-31; git history holds it.

**How a capability gets built, in order:** parse the app key in `AccessoryConfig`
(`vehicle_config.rs`) → emit its `/info` key (`info.rs::build_info`) → echo its token in the SETUP
response `enabledFeatures` (`session.rs`) → implement the behaviour behind it. The first three steps
are the `cornerMasks`/`altScreen` pattern; only the fourth is per-capability work.

`AccessoryConfig` parses 13 keys today: `enablesHEVC`, `enablesViewAreas`, `enablesCornerMasks`,
`enablesLogTransfer`, `enablesMainBufferedAudio`, `appDrivenSetup`, `enablesUIAppearance`,
`enablesMapAppearance`, `enablesFocusTransfer`, `softKeyboard`, `softPhoneKeypad`, `nonMusicLists`,
`musicLists`. Everything below that is not in that list is dropped by serde on ingest — which changes
nothing, because no box implementation sits behind those keys.

| # | Capability | Config key | Wire surface | Status |
|---|---|---|---|---|
| 1 | focusTransfer | `enablesFocusTransfer` (parsed) | per-viewArea `viewAreaSupportsFocusTransfer` | **Advertised**, gated by `levers::focus_transfer()`. No `"focusTransfer"` echo in `enabledFeatures`; no runtime focus give/acquire/offer handling. |
| 2 | logTransfer | `enablesLogTransfer` (parsed) | `/info logTransferInfo` + SETUP `"logTransfer"` | **Advertise tier done, device-proven 2026-08-07.** The chunk transfer itself is unimplemented; the RCS client-type allowlist would have to admit it. |
| 3 | uiContext | not parsed | `/info uiContextURLs`, `uiContextLastOnDisplayURLs`, `uiContextNowOnDisplayURLs`; `changeUIContext` command | **Unbuilt.** |
| 4 | vehicleDataProtocol (VDC route-sharing) | not parsed | RCS DataStream, VDC client types | **Unbuilt.** Blocked on the RCS allowlist-of-one and a VDC handler that does not exist. |
| 4b | iAP2 VehicleStatus / EV telematics | Identify params 6/7 + param 20 (not this key) | iAP2 `0xA100`/`0xA101`/`0xA102`, `0xFFFA`/`0xFFFB` | **Unbuilt.** `message.rs` hardcodes EngineType=Gasoline and omits SupportedChargingConnectors. Blocked on the `ChargingParameter`/`ConsumptionParameter` schema and Maps' `model_id` registry gate. |
| 5 | mainBufferedAudio | `enablesMainBufferedAudio` (parsed) | `/info mainBufferedInfo` (presence-only) + SETUP `"mainBuffered"` | **Phase A only** — advertise + echo, wired-device-proven. Phase B unbuilt: SETUP phase 2 omits MainBuffered/AuxIn/AuxOut, so no buffered stream can arrive. Blocked on the stream-type number and the `mainBufferedInfo` shape, neither captured. Default OFF: advertising without serving is the hazard. |
| 6 | enhancedSiri | not parsed | `/info enhancedSiriInfo` (shape unknown) | **Unbuilt.** Needs the AuxIn(107)/AuxOut(106) audio plane — shared with #5 — plus on-box mic DSP and a keyword/voice-activity detector. |
| 7 | videoPlayback | not parsed | feature token `"videoPlayback"`, `lunaConfig.videoPresetSizes[]` | **Unbuilt and non-code-blocked:** requires Apple to grant `com.apple.developer.carplay-video`. |
| 8 | fileTransfer | not parsed | `/info fileTransferInfo` (shape unknown) | **Blocked-untestable.** Distinct from the iAP2 artwork path the box already implements. Even the advertise step is byte-unsafe without the dict shape. |
| 9 | uiSync / Cluster Control Channel | not parsed | SETUP `"uiSync"` ↔ `enableCarPlayClusterControlChannel` | **Blocked-untestable.** Needs cluster hardware and the CarPlayClusterControl UUID, which is unrecovered. |
| 10 | DCX (spatial audio) | not parsed | `/info DCXEnabled` | **Blocked-untestable.** Would need an APAC multichannel decode + output pipeline that does not exist. The audio ceiling is unchanged: AAC-LC 48 kHz stereo (`06_AV_PIPELINE.md`). |

**Two pieces of shared infrastructure gate several rows.** The RCS DataStream client-type gate is an
allowlist of one (the iAP client type on stream type 130); LogTransfer, the two VehicleDataProtocol
types and CarPlayClusterControl SETUPs are accepted as streams but logged and dropped. Expanding it
once serves rows 2, 4 and 9. The AuxIn/AuxOut/MainBuffered audio plane is shared by rows 5 and 6 and
should be sequenced once.

**One correction worth keeping:** focusTransfer is input-focus handoff between UI zones. It is not
screen-ownership handoff, and a nav event does not pull CarPlay foreground through it — that is
resource borrowing of `kAirPlayResourceID_MainScreen` via `changeModes`/`modesChanged`, which the box
already implements. The host app tooltip conflated the two.

### Cross-references

- **docs/ops/03_REFERENCE_INDEX.md — `../ops/03_REFERENCE_INDEX.md`**: the master index of primary reference sources (licensed
  R14G17 SDK, CarPlay Simulator, CT5 CINEMO, SpeedPlay, iOS 27 extracts) and the decision table for which
  source answers which kind of question. Read it before investigating any protocol question raised here.
- **docs/carplay/04_CAPABILITIES_AND_CONFIG.md — `../carplay/04_CAPABILITIES_AND_CONFIG.md`**: the AccessoryConfig / VehicleConfig field glossary.
  Several toggles in this doc (`enablesDCX`, `enablesUISync`, `enablesFileTransfer`) are flagged
  `[inferred]`/`[I]` there; the dossiers above supersede those entries with the iOS 27 / Simulator
  evidence.
- **docs/carplay/05_METADATA_AND_CONTROLS.md — `../carplay/05_METADATA_AND_CONTROLS.md`**: the authoritative RemoteControlSession
  DataStream transport document (SETUP stream type 130, client-type table + UUIDs, 32-byte header,
  ChaCha20-Poly1305 framing). The load-bearing reference for the logTransfer, VDC route-sharing, uiSync,
  videoPlayback and mainBuffered wire mechanisms above.
- **docs/carplay/06_AV_PIPELINE.md — `../carplay/06_AV_PIPELINE.md`**: the device-proven cornerMasks worked example —
  the two-site declaration arc (`/info` + SETUP `enabledFeatures` echo), the lever-gated
  byte-identical-by-default discipline, and the `accessoryd`-over-USB acceptance proof. Every advertise-tier
  capability above (ranks 1-3, plus the advertise halves of the rest) copies this arc.
- **CLAUDE.md Identify-declaration rules apply to the iAP2 items** (rank 4 EV telematics, param 20 /
  `features.rs`): a `Start*` must be declared with its `Stop*`; a receive must not be declared without its
  send; a subscribe for an id param 6 does not declare is silently ignored. Params 6/7 and the subscribe
  sequence are GENERATED from `features.rs` — never hand-edit one of the three independently (see also
  docs/carplay/05_METADATA_AND_CONTROLS.md). Identify growth must be gated to the wired/AirPlayTunnel arm; the BT-time and wireless Identify
  are byte-pinned.
