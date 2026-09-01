# Open items and roadmap

> **STATUS:** CURRENT · single owner for this topic. Consolidated 2026-08-31 from pre-consolidation docs OPEN, 06; the originals are in git history and in the 2026-08-31 backup. Correct this file in place — do not add a sibling.

Every known-open item in one place, plus the ordered roadmap. An item leaves this file only when it is closed with evidence.


## Settled — do not re-raise

- **`pi/evidence/hostapd_5g.conf` carries a `wpa_passphrase`, and that is accepted.** The values in
  it and in gm_ccpa's head-unit credential are deliberate placeholders for a bench AP; they protect
  nothing. Assessed and accepted by the owner 2026-08-31, explicitly including the case where either
  repo gains a remote. No rotation, no history scrub. Three separate audits have flagged the key
  *name* without reading the value — pattern-matching on `wpa_passphrase` is not a finding.
- **This repo's history carries no assistant-session trailers and no conversation links.** Verified
  2026-08-31 by grepping the whole log for both patterns: zero hits. The sibling gm_ccpa repo has 40
  commits that do; that is tracked there, not here. Write neither pattern out literally in a doc —
  the publication mirror's pre-push hook scans added CONTENT as well as commit messages, so even a
  sentence saying there are none will block the push.

## Open items

<!-- absorbed: ../ops/04_OPEN_ITEMS.md -->

Open work in this project is scattered across a 58-document corpus, most of it inside documents
whose own status line now reads `HISTORICAL` or `CURRENT-WITH-CORRECTIONS`. A reader cannot tell
what is still outstanding without reading all of them. **This is that index — one line per item plus
a back-pointer. It is not a spec and not a plan**: the detail, the reasoning and the wire evidence
stay where they already are, and nothing here supersedes, replaces or closes anything.

**Every item below was re-verified against the working tree on 2026-08-16 by the command shown on
its `Verified open` line.** That verification is the point of the document: the source documents are
up to five weeks old and a large number of their "open" items have since shipped. Items the
documents still call open but the code shows closed are listed separately under
[Found closed](#found-closed--documents-still-call-these-open) — that list is a deliverable, not a
failure. Items that could not be settled either way are under [Unverified](#unverified).

Anchors are **symbols and greppable strings, not line numbers** — line-number rot is this corpus's
single largest source of false claims (see [../ops/04_OPEN_ITEMS.md](../ops/04_OPEN_ITEMS.md)
§DO NOT). Grouping is by subsystem, because a reader wants "what is open in the host app", not
"what does docs/ops/05_AUDITS.md say".

**Scope note.** Owner-decided WONTFIX items are recorded under
[Closed by decision](#closed-by-decision--do-not-re-plan-these) so they are not re-planned as work.
Superseded *design positions* (box-owned `/info` policy, the `/tmp/carplay_metadata` lever as a
canonical control, box-autonomous page-on-boot) are **not** open items and are not listed;
[../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) carries those.

---

### Box daemons — `ocbmd` / `airplayd` / `iap2d` / the supervisor

> **Also see [../ops/04_OPEN_ITEMS.md](../ops/04_OPEN_ITEMS.md) Phase 4**
> for the ten catalogued code defects (the CT_HELLO `out_console` resync, `custom_init.sh`'s
> `/script/start_bluetooth_wifi.sh` call, the missing `iap_role_switch` install). They are
> deliberately not duplicated here so the two documents cannot drift.

- **No inbound `stopSession` handler.** The box declares a five-value `stopSessionReasons`
  vocabulary and then ignores the command; `disconnectReason` plumbing to the supervisor follows
  from the same gap. Both halves sit behind `CARPLAY_SESSION_MGMT`, so shipping the declaration
  without the handler would be worse than not declaring it.
  Source: `docs/carplay/02_SESSION_LIFECYCLE.md` §10 ("the single highest-value fix"). Verified open 2026-08-16:
  `grep -rn "stopSession" crates ccpa` → three hits, all declaration-side (`info.rs`
  `stopSessionReasons`, an `av.rs` comment); nothing in `session.rs`'s `fn command`.

- **`isRemoteControlOnly`, `sessionUUID`, `sessionCorrelationUUID`, `hijackID` are never read.**
  `isRemoteControlOnly` is a protocol-native holding pattern where ours is improvised, and
  `sessionCorrelationUUID` is confirmed live on the wire.
  Source: `docs/carplay/02_SESSION_LIFECYCLE.md` §10. Verified open 2026-08-16:
  `grep -rn "isRemoteControlOnly\|sessionWillBeHijacked\|sessionCorrelationUUID\|hijackID" crates ccpa host`
  → zero hits; `sessionUUID` appears only in a doc comment.

- **P2 lifecycle: `CT_SESSION_EVENT` was never widened to `[state][reason]`.** Every emitter and
  every consumer on all three hosts still sends and parses exactly two bytes, so the box cannot
  stream real lifecycle state or a failure reason to the app.
  Source: `docs/carplay/02_SESSION_LIFECYCLE.md` §Phasing, [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-11-1`. Verified open 2026-08-16:
  `grep -rn "CT_SESSION_EVENT" --include="*.rs" --include="*.swift" --include="*.kt" .` → all
  emitters are `&[p::CT_SESSION_EVENT, sev]`, all consumers gate on `l >= 2`.

- **P2 lifecycle: no single reason-carrying finalize.** `AvSession::teardown` takes only the request
  plist and `reset()` only `&self` — no `OSStatus`-style reason is threaded from clean stop, failure
  or `Drop`, so a failed teardown is indistinguishable from a clean one.
  Source: `docs/carplay/02_SESSION_LIFECYCLE.md` §"Supporting layers", `R-11-1`. Verified open 2026-08-16:
  `grep -n "fn teardown" crates/vendor/receiver/src/session.rs` → no reason argument on either arm.

- **P2 lifecycle: the `ccpad` Rust supervisor daemon does not exist.** Supervision is still the
  hardened shell loop; the strategic endpoint (dependency DAG, real `fork`/`waitpid`, one
  `::respawn:` entry) has no crate and no binary.
  Source: `docs/carplay/02_SESSION_LIFECYCLE.md` §"Formalization & strategic direction", `R-11-1`. Verified open 2026-08-16:
  `grep -rn "ccpad" --include="*.rs" --include="*.sh" --include="*.toml" .` → zero hits.

- **Multi-second blocking I2C still runs with the tunnel's `SESSION` mutex held.** The structural fix
  (compute the action, drop the guard, run the MFi op, re-acquire, re-validate) was never written, so
  a chip stall blocks the RCS reader, the event handler, `POST /command` and teardown.
  Source: `docs/ops/05_AUDITS.md` §8 "Still open". Verified open 2026-08-16:
  `grep -n "SESSION\|execute(" crates/vendor/receiver/src/iap_tunnel.rs` → `execute` is called inside
  the `plock(&SESSION)` guard and calls `mfi_retry("cert", …)` straight into the chip.

- **Artwork duplicate-fragment corruption — the iAP2 link layer still has no duplicate-sequence
  suppression.** An over-length buffer is emitted as a corrupt JPEG and acknowledged `Success`, with
  only an `[art] OVERLONG` log line; the fix was designed and deferred to a Phase 2 that never ran.
  Source: `docs/ops/05_AUDITS.md` §4, `docs/ops/05_AUDITS.md` §"Where the agents corrected the audit" (I2). Verified open
  2026-08-16: `grep -ni "duplicate\|dup_seq\|seen_seq" crates/vendor/iap2-core/src/link.rs` → no
  dedupe; `peer_seq` is only assigned.

- **`CT_INPUT_NACK` was never built.** The box still silently discards `CH_INPUT` while the host is
  GONE, with no instant notification back to the host.
  Source: `docs/ops/05_AUDITS.md` §"Also deferred", `docs/wireless/01_BT_AND_RADIO.md` §3. Verified open 2026-08-16:
  `grep -rn "CT_INPUT_NACK\|INPUT_NACK" crates ccpa host` → zero hits.

- **`connect_seam` still resolves through `to_socket_addrs()` rather than numeric-only.** Deferred
  pending a host-app `CH_IP` usage check; the DNS-capable resolver path is unchanged.
  Source: `docs/ops/05_AUDITS.md` §"Also deferred". Verified open 2026-08-16:
  `grep -n "fn connect_seam" -A6 ccpa/ocbmd/src/main.rs` → `target.to_socket_addrs().ok()?.next()?`.

- **`av_dropped` / `lo_dropped` are stderr-only.** The app's CCPA tab polls `MGMT_GET_INFO`, so the
  two backpressure counters that matter most for diagnosing A/V loss are invisible to the only UI
  that polls the box.
  Source: `docs/carplay/02_SESSION_LIFECYCLE.md` §3. Verified open 2026-08-16:
  `grep -n "fn box_info_json" -A45 ccpa/ocbmd/src/main.rs` → neither counter is in the emitted JSON.

- **`wireless_down` orphans the bring-up's forked children.** Both teardown branches `pkill` only
  `carplay-wireless`, `airplayd` and `rx-connect`; `wlan_on.sh`, `bt_on.sh` and
  `attach_bluetooth.sh` survive. The obvious `pkill` fix is the IW416 wedge the file warns about
  elsewhere, so a lock is probably the right shape.
  Source: `../ops/04_OPEN_ITEMS.md` §5 (#28). Verified open 2026-08-16:
  `grep -n "pkill" tools/session_supervisor.sh` → 20 hits, none naming `wlan_on.sh`, `bt_on.sh` or
  `attach_bluetooth.sh`; the `wireless_down` teardown reaps only the three daemons.

- **The sub-16-byte opcode-0 decrypt lane never advances its counter.** A `<16` byte VideoFrame body
  passes through as plaintext without incrementing `counter`, so if that lane ever fires the box and
  host nonce counters desync permanently. Only the host half of the fix landed.
  Source: `docs/ops/05_AUDITS.md` §"Where the agents corrected the audit" #3. Verified open 2026-08-16:
  `grep -n "counter += 1" crates/vendor/receiver/src/session.rs` → exactly one occurrence, inside the
  `if body_size >= 16` arm of the `0 =>` opcode match; the `else` arm returns `body.to_vec()` with no
  counter advance.

- **Resource arbitration is a one-shot `takeScreen` at RECORD and is never revisited.** There is no
  untake, no release and no re-negotiation, where both reference implementations model this as a
  stateful two-party negotiation.
  Source: `docs/carplay/05_METADATA_AND_CONTROLS.md` §II.2, `docs/carplay/02_SESSION_LIFECYCLE.md` §Summary item 2. Verified open 2026-08-16:
  `grep -rn "send_take_screen" crates ccpa` → one definition, one call site, no untake emitter.

- **`send_command` does not enforce Apple's `sessionStarted` gate.** Deliberate and documented, but a
  known deviation from `AirPlayReceiverSession.c`'s `require_action_quiet(inSession->sessionStarted, …)`
  that has never been tested either way — and one of the two surviving explanations for the
  `limitedUI` no-op.
  Source: `docs/carplay/03_SDK_GROUND_TRUTH.md` §10 ("untested rather than refuted"). Verified open 2026-08-16:
  `grep -n "session_started" crates/vendor/receiver/src/events.rs` → checked only inside
  `modes_changed_tunnel_nudge()`; `pub fn send_command` has no check.

- **The `airplayd → :9001 → ocbmd` forward path is still local TCP loopback.** The copy elimination
  (unix socket / `splice`) that the 4K@60 track lists as "next" has not started.
  Source: `docs/host/00_MACOS_HOST_APP.md` §"4K@60 optimization track" item 4. Verified open 2026-08-16:
  `grep -n "TcpListener\|UnixListener" ccpa/ocbmd/src/main.rs` → all five A/V seams are
  `TcpListener::bind(("127.0.0.1", port))`; no `UnixListener`, no `splice`.

---

### CarPlay capability surface — the app-pushed config the box does not yet act on

> The single best machine-readable inventory of this workstream is the `EMITTED_BUT_UNREAD` constant
> asserted by `every_emitted_key_is_parsed_or_knowingly_ignored` in
> `crates/vendor/receiver/src/vehicle_config.rs`. It names every key both host apps emit that nothing
> on the box reads. Grep that first.

- **Seven `accessoryConfig` capability keys are still dropped by the box serde gate.**
  `enablesVideoPlayback`, `enablesEnhancedSiri`, `enablesUIContext`, `enablesUISync`,
  `enablesFileTransfer`, `enablesVehicleDataProtocol` and `enablesDCX` reach the box in YAML from
  both host apps and are silently discarded. Adding a field is a one-line formality — it must only
  land in the commit that makes the box act on it.
  Source: `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` TL;DR, [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-53-2`. Verified open 2026-08-16:
  `grep -n "serde(default, rename" crates/vendor/receiver/src/vehicle_config.rs` → `AccessoryConfig`
  declares exactly nine keys, and the same seven are listed under `EMITTED_BUT_UNREAD`.

- **`focusTransfer` is advertise-half-armed — no SETUP `enabledFeatures` echo.** The lever and the
  per-view `/info` flag landed, but no SETUP author pushes the `"focusTransfer"` token, so iOS can
  never negotiate it; runtime focus (`accessoryAcquireFocus` / `deviceOfferFocus` /
  `initialFocusOwner`) is entirely absent.
  Source: `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` §1, `R-49-4`. Verified open 2026-08-16:
  `grep -n "feats.push" crates/vendor/receiver/src/session.rs` → eight tokens, none of them
  `focusTransfer`; the same six-token list appears in `VehicleConfig.swift` and `setup_driver.rs`.

- **`uiContext` is completely unimplemented on the box.** No serde field, no lever, no
  `uiContext*URLs` keys in `/info`, no echo, no `changeUIContext` emitter — only an app UI stub.
  Source: `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` §3, `docs/carplay/03_SDK_GROUND_TRUTH.md` §5. Verified open 2026-08-16:
  `grep -rn "uiContext\|changeUIContext" --include="*.rs" crates ccpa` → zero hits outside
  `vehicle_config.rs` test fixtures.

- **The RCS stream-130 client-type gate is still an allowlist of one.** LogTransfer, the two
  VehicleDataProtocol types and a CarPlayClusterControl SETUP are accepted as streams but their
  frames are logged and dropped. Expanding it once unblocks three capabilities below.
  Source: `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` §"Shared infrastructure" 2. Verified open 2026-08-16:
  `grep -n "IAP_CLIENT_TYPE" crates/vendor/receiver/src/session.rs` → one UUID constant is the only
  accept condition, with a doc comment reading "The gate is an allowlist of one".

- **`mainBufferedAudio` Phase B (accept + forward the stream) is unbuilt.** Phase A advertise+echo
  landed and is wired-device-proven; SETUP phase 2 still falls through to the "NOT IMPLEMENTED —
  omitted" arm for MainBuffered/AuxIn/AuxOut, so no buffered audio can arrive. The true
  `mainBufferedInfo` shape and the MainBuffered stream-type number are still unknown, and the
  wireless arm was never captured.
  Source: `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` §5, `docs/ops/04_OPEN_ITEMS.md` step 6. Verified open 2026-08-16:
  `grep -n "Still unimplemented and therefore omitted" -A3 crates/vendor/receiver/src/session.rs`.

- **`vehicleDataProtocol` / VDC route-sharing is entirely unbuilt.** The box advertises neither
  `vehicleStateProtocol` nor `vehicleStateProtocolInfo`, never echoes the token, and no
  VDCSchema-driven characteristic server exists.
  Source: `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` §4 Mechanism A, `docs/host/00_MACOS_HOST_APP.md` Tier 4. Verified open 2026-08-16:
  `grep -rn "vehicleStateProtocol" crates ccpa --include="*.rs"` → no hits in box code.

- **`videoPlayback` is inert and blocked on an Apple-approval-only entitlement.** No lever, no
  `/info` advertisement, no `lunaConfig`/`videoPresetSizes`, no echo. The non-code action is
  requesting `com.apple.developer.carplay-video`; the `/info` wire shape is also uncaptured.
  Source: `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` §7. Verified open 2026-08-16:
  `grep -rn "videoPlayback\|lunaConfig\|videoPresetSizes" --include="*.rs" crates ccpa` → only
  `vehicle_config.rs` test fixtures.

- **`fileTransfer` is blocked-untestable — even the advertise step is byte-unsafe.** The
  `fileTransferInfo` dict's keys are unknown, as is which transport carries the chunk dict.
  Unrelated to the iAP2 session-2 artwork assembler the box already implements.
  Source: `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` §8. Verified open 2026-08-16:
  `grep -rn "fileTransferInfo" crates ccpa host` → zero hits outside docs.

- **`uiSync` / the CarPlay Cluster Control Channel is blocked, and the cheap UUID-harvest probe has
  not been run.** The recommended low-risk probe — advertise only, then read the box's own
  stream-130 SETUP log line for the unknown `clientTypeUUID` — turns the biggest unknown into a
  captured fact in one connection and is still untaken.
  Source: `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` §9 Step A. Verified open 2026-08-16:
  `grep -rn "uiSync\|ClusterControl" --include="*.rs" crates ccpa` → only test fixtures.

- **DCX is investigation-only — no spatial/APAC pipeline exists anywhere in the tree.** The acronym,
  the `/info` placement and the exchange's wire shape are all open; the doc says write no code
  before the iOS-27 disassembly lands.
  Source: `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` §10. Verified open 2026-08-16:
  `grep -rn "DCXEnabled\|APAC\|spatial" --include="*.rs" crates ccpa` → only test fixtures.

- **`nightMode` and `rightHandDrive` are written by the app and silently dropped by the box.** Both
  are on `EMITTED_BUT_UNREAD` with a comment conceding they "READ LIKE REAL SETTINGS and are worth a
  decision rather than an entry here". Neither is parsed into a config field nor emitted in `/info`;
  the live night-mode path is the runtime `setNightMode` command, not the config key.
  Source: `docs/carplay/03_SDK_GROUND_TRUTH.md` §4, `docs/carplay/03_SDK_GROUND_TRUTH.md` via `R-26-1`. Verified open 2026-08-16:
  `grep -n "nightMode\|rightHandDrive" crates/vendor/receiver/src/vehicle_config.rs crates/vendor/receiver/src/info.rs`
  → hits only in a test YAML fixture and in the `EMITTED_BUT_UNREAD` list; **zero hits in `info.rs`**,
  and neither is a serde field.

- **`altDisplayPanels[]` is parsed but never emitted; `showsInstruments` and `initialURL` are
  hardcoded box constants on a legacy `displays[]` entry.** This is a doctrine gap (box-decided where
  the app authors the value). **Caveat, do not lose it:** the code's own comment records docs/carplay/03_SDK_GROUND_TRUTH.md §5's
  "root cause" framing as REFUTED — cluster content already works via `showUI` query parameters — so
  anyone reviving the emission must first establish what it adds.
  Source: `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` §Roadmap item 4, `docs/carplay/03_SDK_GROUND_TRUTH.md` §5, `R-26-2`, `R-49-2`. Verified open 2026-08-16:
  `grep -rn "alt_display_panels\|displayPanels" crates/vendor/receiver/src/{vehicle_config,info}.rs`
  → parsed with the comment "PARSED since 2026-08-10; still not emitted"; `info.rs` has no
  `displayPanels` key and a test named `alt_display_panels_are_parse_only_today`.

- **Only one view area is ever emitted; `adjacentViewAreas` is hardcoded empty.** Static
  multi-view-area, runtime `ViewAreaUpdate` switching, `updateDisplayPanels` live re-negotiation and
  the type-112 second cluster are all unimplemented, though multiple `viewAreas[]` already parse.
  Source: `docs/carplay/06_AV_PIPELINE.md` §2–§4, `docs/carplay/03_SDK_GROUND_TRUTH.md` §10, `docs/carplay/02_SESSION_LIFECYCLE.md` §"Apple's session/lifecycle model". Verified
  open 2026-08-16: `grep -n "initialViewArea\|adjacentViewAreas" crates/vendor/receiver/src/info.rs`
  → both unconditional `Integer(0)` / empty `Array`; `grep -rn "updateDisplayPanels\|updateViewArea" crates ccpa host`
  → only two log-classifier strings in `MetadataWindow.swift`.

- **`displays[].features` is a hardcoded `if levers::dpad() { 0x1A } else { 0x0A }`, and four parsed
  `hidConfig` keys drive nothing** (`touchpadSupport`, `steeringWheelSupport`, `mediaButtonsSupport`,
  `touchScreenMode`). `0x02` claims Knobs and `0x10` claims Touchpad with no backing `hidDevices[]`
  entry, while `dPadSupport` — the only input the emission consults — contributes nothing to that
  word in Apple's own mapping. The current value is hardware-validated, so correcting it is a wire
  change needing a session. Tracked as workstream C-7/C-8.
  Source: `docs/carplay/03_SDK_GROUND_TRUTH.md` §8 item 5, `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` §3. Verified open 2026-08-16:
  `grep -n "disp_features" crates/vendor/receiver/src/info.rs`;
  `grep -rn "touchpad_support\|steering_wheel_support\|media_buttons_support\|touch_screen_mode" --include="*.rs" crates ccpa`
  → every hit is a struct field or a test.

- **HID uids are hardcoded 1–5; Apple's runtime allocator is not replicated.** The descriptors
  themselves are done and byte-verified, but `steeringWheelID` consumes an ID while emitting no
  device, and the uid-4/uid-5 entries still await hardware validation.
  Source: `docs/carplay/03_SDK_GROUND_TRUTH.md` §8 item 4 and §2 trap 1. Verified open 2026-08-16:
  `grep -n "HID_UID" crates/vendor/receiver/src/info.rs` → five `const` literals, no counter and no
  allocator.

- **Video lanes are fixed and capped at two.** `altVideoStreams[]` parses as an array but only its
  first entry is used, and the video seam's key message carries an `scid` the host discards; a third
  stream would need new channel ids or scid tagging. Audio has no such limit.
  Source: `docs/carplay/01_OCBM_PROTOCOL.md` §"What is genuinely not supported (open item)". Verified open 2026-08-16:
  `grep -n "CH_VIDEO\|CH_ALT_VIDEO" crates/ocbm-proto/src/lib.rs` → exactly two video channels.

- **Accessory-side NMEA/GPS is parse-only.** The box decodes the phone's `StartLocationInformation`
  selection but nothing ever sends `LocationInformation` (0xFFFB).
  Source: `docs/host/00_MACOS_HOST_APP.md` Tier 4. Verified open 2026-08-16:
  `grep -rn "0xFFFB\|MSG_LOCATION_INFORMATION" --include="*.rs" crates ccpa` → the spec table and
  `crates/vendor/metadata/src/location.rs` (a parser); no emitter.

---

### iAP2 & metadata

> **Also see [../ops/04_OPEN_ITEMS.md](../ops/04_OPEN_ITEMS.md) Phase 4**
> for the two catalogued `iap2-core/src/message.rs` defects (the params-6/7 "closed dead end as a
> class" self-contradiction, and the hardcoded Identify serial).

- **Identify param 30 declares only subs 0 and 1, so `lane_guidance` is inert and road/destination
  names cannot populate.** Apple's sub 8 `MaxLaneGuidanceStorageCapacity` reads "Must be included to
  receive Lane Guidance instructions", and subs 2/3 gate `CurrentRoadName`/`DestinationName`.
  **⚠️ CONTRADICTED BY OUR OWN CAPTURE — see [False claims](#false-claims-found-while-verifying).**
  Source: `docs/carplay/03_SDK_GROUND_TRUTH.md` §8 item 1. Verified open 2026-08-16:
  `grep -n "route_guidance as sub" -A8 crates/vendor/iap2-core/src/message.rs` → the emitter pushes
  only `sub::IDENTIFIER` and `sub::NAME`.

- **Param 30 is still UNGATED across all three transports, which blocks its expansion.** Growing it
  today also grows the byte-pinned BT-time `Wireless` Identify; the doc states gating to
  `AirPlayTunnel` first is "a prerequisite, not an option".
  Source: `docs/carplay/03_SDK_GROUND_TRUTH.md` §8 item 2. Verified open 2026-08-16:
  `grep -n "ROUTE_GUIDANCE_DISPLAY_COMPONENT" crates/vendor/iap2-core/src/message.rs` → the emitter
  `push(…)` sits in a bare block with no `matches!(transport, …)` guard; the gate exists only as a
  commented-out `ROLLBACK` suggestion above it.

- **C-3 — the app-pushed vehicle identity is parsed and resolved but nothing on the wire reads it.**
  `build_ident_info_with` has no production caller and `iap2d` still passes
  `message::accessory_name("CarLink")` at both Identify sites.
  Source: `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` §3 row C, `../ops/04_OPEN_ITEMS.md` §4. Verified open 2026-08-16:
  `grep -rn "build_ident_info_with" crates ccpa` → hits only inside a `#[test]` in `config.rs`;
  `grep -n "accessory_name" ccpa/iap2d/src/main.rs` → two hardcoded call sites.

- **C-4 — `features.rs` structurally cannot express vehicle status, which blocks C-5 (param 21).**
  `0xA101` is accessory-sourced while `0xA100`/`0xA102` are device-sourced, inverted versus every
  existing feature, so a naive `Trigger::Subscription` trips the direction-correctness test. The code
  carries a hard DO-NOT-ENABLE warning, and a `0x1D03` is unrecoverable within a session.
  Source: `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` §4 Mechanism B, `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` §3. Verified open 2026-08-16:
  `grep -rn "0xA100\|0xA101\|0xA102" crates/vendor/iap2-core/src/features.rs` → zero hits.

- **What flips `CarPlayAppListAvailable` from 0 Unknown to 2 Accepted is still UNDETERMINED, and the
  proposed Simulator experiment was never run.** It needs no box work. Related known difference,
  recorded not resolved: we omit sub 1 `CarPlayAppListMax` where Apple's Simulator always emits it.
  Source: `docs/carplay/03_SDK_GROUND_TRUTH.md` §6, corroborated by `docs/ops/05_AUDITS.md` §2 ("`0xAD01` remains uncaptured"). Verified open
  2026-08-16: `grep -n "CarPlayAppListAvailable" crates/vendor/iap2-core/src/features.rs` → still
  reads "remains UNDETERMINED by every normative source"; no matching capture in `docs/ops/captures/`.

- **Nine `communications` (0x4158) capability keys are emitted by the box and read by nothing.**
  `cellularSupported`, `faceTimeAudioEnabled`, `initiateCallAvailable`, `endAndAcceptAvailable`,
  `holdAndAcceptAvailable`, `swapAvailable`, `mergeAvailable`, `holdAvailable`,
  `faceTimeVideoEnabled`. The raw-record fallback renders the payload, so this is a pane-mapping gap,
  not data loss.
  Source: `docs/ops/05_AUDITS.md` §1 ("Live gap"). Verified open 2026-08-16:
  `grep -rn "cellularSupported\|faceTimeAudioEnabled\|initiateCallAvailable" host/CarPlayHost/` →
  zero hits; the same grep over `--include="*.rs"` hits `iap2-core/src/metadata.rs`.

- **W9 — the accessory BT MAC in the byte-pinned Identify is a hardcoded constant.** Param 17 is
  fleet-identical rather than read from the box's own `hci0`. Reclassified to its own pinned-Identify
  hardware session with a ready revert; that session never ran.
  Source: `docs/ops/05_AUDITS.md` §"Where the agents corrected the audit" #1, §"Hardware session 2". Verified open
  2026-08-16: `grep -rn "ACCESSORY_BT_MAC" crates/vendor/wireless/src` → a fixed 6-byte literal in
  `bt_driver.rs` used at both Identify sites.

- **`0x4E0D WirelessCarPlayUpdate` and `0x5700`–`0x5703` are declared but not acted on.**
  Source: `docs/carplay/02_SESSION_LIFECYCLE.md` §10. Verified open 2026-08-16:
  `grep -rn "0x4E0D" --include="*.rs" crates ccpa` → every hit is a spec-table entry or a declaration
  list; no handler.

---

### Wireless transport — the RCS DataStream and the BT→WiFi path

- **An inbound RCS `'sync'` obliges a `'rply'`; the box logs and does not send one — BLOCKED, not
  merely unstarted.** `_controlServer_sendResponseInternal` writes `messageType = 0` while the
  phone's only RCS inbound dispatch accepts `'cmnd'`/`'died'`; both cannot be true, and a malformed
  accessory frame has already wedged this link once, so guessing wrong is asymmetrically expensive.
  Source: `docs/carplay/05_METADATA_AND_CONTROLS.md` §5 item 1, `../ops/04_OPEN_ITEMS.md` §5 (#15b). Verified open
  2026-08-16: `grep -n "sync" crates/vendor/receiver/src/session.rs` → logs
  `inbound 'sync' … expects a 'rply' — NOT IMPLEMENTED`; `KIND_RPLY` has no non-test consumer.

- **The tunnel SYN declares 2 link sessions at session version 1 where Apple's transport-type-2
  template declares 3 at version 2, and post-`Identified` frames bypass `state::on_message`.** Both
  were deferred so the first hardware test had one variable; that test has since run. A re-`0x1D00`,
  re-`0xAA00` or `0xAA04` after Identify falls through to the generic dispatcher.
  Source: `docs/carplay/05_METADATA_AND_CONTROLS.md` §5 items 2–4. Verified open 2026-08-16:
  `grep -n "SYN_PARAMS_ZERO_ACK_TUNNEL" -A2 crates/vendor/iap2-core/src/link.rs` → leading version
  byte 1 and two 3-byte session blocks.

- **The event capture still watches the `POST /command` carrier that docs/carplay/05_METADATA_AND_CONTROLS.md refuted.**
  `/tmp/carplay_event_capture.bin` is written from the event-channel plist path, a channel now proven
  not to carry wireless iAP2 (that rides the RCS DataStream, stream type 130).
  Source: `docs/ops/05_AUDITS.md` §8 "Still open". Verified open 2026-08-16:
  `grep -rn "carplay_event_capture" --include="*.rs" crates` → one hit, inside
  `events.rs::handle_inbound_event`'s RTSP-body path; no equivalent in `datastream.rs`.

- **`/tmp/carplay_transport` is still claimed at `ensure_av_layer` entry rather than in the
  `airplayd_up` success branch.** The flag is written before the spawn is confirmed, with a rollback
  on failure instead.
  Source: `docs/ops/05_AUDITS.md` §"2026-08-01" Finding B. Verified open 2026-08-16:
  `grep -n "carplay_transport" crates/vendor/wireless/src/av.rs` → written at the top of
  `ensure_av_layer`, rolled back only in the `else` of `if airplayd_up`.

- **An absent `clientTypeUUID` is treated as iAP, diverging from Apple's teardown path.** Apple's
  `_DataStreamSessionSetup` sends an absent UUID to `-6735` teardown identically to an unrecognised
  one. Knowingly kept as permissive-but-harmless; listed only so the index is complete.
  Source: `docs/carplay/03_SDK_GROUND_TRUTH.md` §3. Verified open 2026-08-16:
  `grep -n "clientTypeUUID" crates/vendor/receiver/src/session.rs` →
  `let is_iap = client_type.is_empty() || client_type.eq_ignore_ascii_case(IAP_CLIENT_TYPE);`.

- **`iAPChannel` and `sessionManagement` were never migrated to per-transport app config.** Both are
  still env-gated box-only SETUP tokens with no config key, and the host's phase-1 authoring must
  UNION them back in from the box's own response or silently kill the wireless iAP2 tunnel.
  Source: `docs/ops/04_OPEN_ITEMS.md` §Building blocks step 8 (workstream C/D debt). Verified open 2026-08-16:
  `grep -n "hostAuthorableFeatures" -A4 host/CarPlayHost/carlink_macOS/OCBM/AirPlaySetupSession.swift`
  → six tokens, neither of the two present.

- **Pair-RESUME is absent.** Only pair-setup and pair-verify exist; there is no persisted-session-id
  resume and no resume→verify fallback, so a known device always pays the full pair-verify round trip.
  Source: `docs/carplay/02_SESSION_LIFECYCLE.md` §"Apple's session/lifecycle model", `R-11-1`. Verified open 2026-08-16:
  `grep -rni "pair-resume\|pair_resume\|pairResume" --include="*.rs" --include="*.swift" --include="*.kt" .`
  → zero hits.

- **The Siri and TEARDOWN fixes from the docs/carplay/03_SDK_GROUND_TRUTH.md pass have never been individually exercised on
  hardware.** Structurally verified only; the third member of that trio (cluster content) has since
  been hardware-confirmed. Given `R-49-7` — that same pass silently killed wireless metadata for ten
  days because whole-session health was read from A/V alone — the remainder deserves its own session.
  Source: `docs/carplay/03_SDK_GROUND_TRUTH.md` §10, `R-49-7`. Verified open 2026-08-16:
  `ls crates/vendor/receiver/tests/` → `control.rs net.rs r4_c2_schema.rs setup_stream_130.rs`; no
  Siri or TEARDOWN test and no capture named for either.

---

### Radios, box identity & bring-up

- **Broadcom and NXP mappings are refused, not resolved — those units report `unsupported` and get no
  wireless.** Closing it means slicing the dispatcher to the unit's own SDIO branch and resolving
  that branch's literal `var=value` assignments. Emitting the raw text instead is the documented
  silent-false-success failure (docs/wireless/01_BT_AND_RADIO.md §6d).
  Source: `docs/wireless/01_BT_AND_RADIO.md` §7. Verified open 2026-08-16:
  `grep -n "safe_cmd" -A8 ccpa/rootfs/script/radio_detect.sh` → the refusal is still
  `case "$_c" in *'$'*|*'`'*|…) echo "" ;;`, which blanks any vendor line carrying an unexpanded
  variable; no closed-form Broadcom/NXP mapping has been added beside it.

- **Broadcom is entirely untested, and RTL8733BS / SD8987 are unexercised.** The seam resolves them
  on paper from their own dispatchers — that is the whole design claim — but the claim is measured
  only on 0xc822. On Broadcom, `wlan0` does not exist until explicitly created on top of `sta0`, and
  no Broadcom hardware is available to this project.
  Source: `docs/wireless/01_BT_AND_RADIO.md` §7, and §5's hardware-validated results (RTL8822CS only). Verified open
  2026-08-16: `grep -n "c822" ccpa/rootfs/script/radio_detect.sh` → `0xc822) CHIP=realtek_rtl8822cs`
  is the id §5 validated; this is a hardware-coverage gap, so no grep can close it.

- **`RADIO_BT_PRELOAD_CMD` is consumed by `radio_hal.sh` but never emitted by `radio_detect.sh`.** A
  mapped-path unit needing `fw_loader_linux` before BT attach would silently never run it. Moot while
  IW416 resolves to the `owned` backend, latent otherwise.
  Source: `docs/wireless/01_BT_AND_RADIO.md` §7. Verified open 2026-08-16:
  `grep -rn "RADIO_BT_PRELOAD_CMD" ccpa/ tools/ pi/` → three hits, all in `radio_hal.sh`; zero in
  `radio_detect.sh`.

- **"One box, one name" is not achieved — the Rust side never reads `/etc/carplay_ident` and still
  hardcodes `wlan0`.** `carplay-wireless`, `bt_on.sh` and `ocbmd`'s `bt_name_from()` each derive a
  name independently, and because the supervisor execs `carplay-wireless` after `radio_hal.sh bt_on`
  the controller advertises `CarLink-<suffix>` rather than the seam's `ccpa-<4hex>`. The suffixes can
  diverge too, not just the prefixes.
  Source: `docs/wireless/01_BT_AND_RADIO.md` §6c/§7, `R-57-1`. Verified open 2026-08-16:
  `grep -rn "carplay_ident" ccpa crates tools pi host` → four hits, all in
  `radio_hal.sh`/`radio_ap_up.sh`, none in Rust; `grep -n "wlan0" crates/vendor/wireless/src/av.rs crates/vendor/wireless/src/box_identity.rs crates/vendor/iap2-core/src/message.rs ccpa/ocbmd/src/main.rs`
  → `const WLAN_IFACE: &str = "wlan0"` plus three `/sys/class/net/wlan0/address` reads.

- **The hardcoded Raspberry Pi BT MAC in `bt_driver.rs` is unparameterised.** Same identity-chain
  workstream as the item above.
  Source: `docs/wireless/01_BT_AND_RADIO.md` §7. Verified open 2026-08-16:
  `grep -n "ACCESSORY_BT_MAC" crates/vendor/wireless/src/bt_driver.rs` → a fixed literal.

- **There is no reversible radio verification GATE in the baseline, run while still on NCM.** The
  baseline installs the seam and *reports* the resolved platform, but nothing gates the install on a
  pass/fail radio result or reverts on failure.
  Source: `docs/wireless/01_BT_AND_RADIO.md` §7. Verified open 2026-08-16:
  `grep -n "radio_detect" tools/ncm_base_install.sh` → the only invocation pipes
  `sh /script/radio_detect.sh` into a `grep -E "CHIP|SDIO_DEVICE|…"` summary print, with no exit-code
  test and no revert path.

- **`ocbm_install.sh` has no generic manifest cross-reference assertion.** The specific Realtek
  failure it would have caught is closed (`--full` now ships the seam alongside the supervisor), but a
  future shipped script referencing a path the target lacks would still slip through.
  Source: `docs/wireless/01_BT_AND_RADIO.md` §7. Verified open 2026-08-16:
  `grep -n "assert" tools/ocbm_install.sh` → one hit, a comment about the inittab; `manifest()` is a
  flat `local|remote|mode` list consumed by `push_file` with no cross-reference check.

- **The startup decode transient described in `docs/wireless/01_BT_AND_RADIO.md` §6b is unexplained and unfixed.**
  Source: `docs/wireless/01_BT_AND_RADIO.md` §7. Verified open 2026-08-16: no code carries a fix or a tracking marker —
  `grep -rn "decode transient\|startup transient" crates ccpa` → zero hits.

---

### macOS host app

> **Also see [../ops/04_OPEN_ITEMS.md](../ops/04_OPEN_ITEMS.md) Phase 4**
> for the three catalogued host-app defects (`fn do_hello`'s caps-constant nonce, `sendHello()`'s
> zeros, and `inertKeys` being wrong in both directions).

- **`AppDelegate.carPlayView(_:didMultiTouchTwo:)` is an empty stub — pinch and two-finger scroll are
  captured and dropped.** `CarPlayView` emits the delegate from nine sites; `AppDelegate` implements
  it with `{}`. **Its stated cause in docs/host/00_MACOS_HOST_APP.md is WRONG:** the box ships Apple's two-finger descriptor
  with a unit test against Apple's fill order, so this is host-side work, not box work.
  Source: `docs/carplay/06_AV_PIPELINE.md` §5 Phase 2, `docs/host/00_MACOS_HOST_APP.md` Tier 1 #5, `R-15-1`, `R-17-1`. Verified open 2026-08-16:
  `grep -rn "didMultiTouchTwo" host/CarPlayHost/carlink_macOS/App/*.swift` → nine emit sites in
  `CarPlayView.swift`, one implementation in `AppDelegate.swift` with an empty body.

- **The macOS app pushes no `hidConfig.touchScreenSupportsMultiTouch`, so the box's two-finger
  descriptor is never advertised under a macOS-pushed config.** This is the second, independent half
  of the multi-touch gap: even with the delegate implemented, `levers::multi_touch()` would be off.
  The Android host does emit the key.
  Source: `docs/carplay/06_AV_PIPELINE.md` §2. Verified open 2026-08-16:
  `grep -rn "touchScreenSupportsMultiTouch" --include="*.swift" --include="*.kt" host/` → the two
  Android emitters and zero hits anywhere under `host/CarPlayHost/`.

- **Touch aspect is still derived from the advertised/persisted resolution, not from the decoded
  frame.** `CarPlayView.videoAspect` — which defines `videoRect` and therefore every normalized touch
  coordinate — is seeded and updated only from `VehicleConfigModel.persistedMainResolution()`.
  Source: `docs/host/00_MACOS_HOST_APP.md` Tier 2 #8. Verified open 2026-08-16:
  `grep -rn "updateVideoAspect" host/CarPlayHost/carlink_macOS --include="*.swift"` → one caller,
  `MainWindowController.applyResolution`, itself driven by `persistedMainResolution()`.

- **The USB read loop calls `ClearPipeStallBothEnds` on every transaction timeout and has no retry
  backoff.** The audit asked for the clear only on a real `kIOReturnPipeStall`. The other half of the
  item — an idle timeout wrongly counting toward the 5-error disconnect streak — is fixed.
  Source: `docs/host/00_MACOS_HOST_APP.md` Tier 2 #10, `R-17-1`. Verified open 2026-08-16:
  `grep -n "ClearPipeStallBothEnds\|kIOReturnPipeStall" host/CarPlayHost/carlink_macOS/USB/USBTransport.swift`
  → three unconditional clears (including inside the `kr == Self.kUSBTransactionTimeout` arm) and zero
  `kIOReturnPipeStall` discrimination; both arms `continue` with no sleep.

- **The bounded resubscribe with an atomic `OCBMAVDecrypt.reset()` never landed.** `OCBMAVDecrypt` has
  no `reset()` at all, so a resubscribe cannot re-lockstep the ChaCha counter; the retry rides the
  1 Hz heartbeat and is bounded only by session lifetime.
  Source: `docs/carplay/02_SESSION_LIFECYCLE.md` §"Supporting layers" (P1 residue), `R-11-1`. Verified open 2026-08-16:
  `grep -n "func reset" host/CarPlayHost/carlink_macOS/OCBM/OCBMAVDecrypt.swift` → zero hits.

- **The session log still carries no build stamp — only `Version: 1.0`.** docs/ops/05_AUDITS.md's own conclusion
  called this "the cheapest fix here" after three sessions were lost to a stale binary; it never
  landed.
  Source: `docs/ops/05_AUDITS.md` §1. Verified open 2026-08-16:
  `grep -rn "CFBundleVersion\|buildStamp\|buildDate" host/CarPlayHost/carlink_macOS/App/` → nothing;
  `FileLogger.preamble()` prints `CFBundleShortVersionString` only.

- **`CT_RADIO` (0x16) is complete box-side but has no app-side caller.** Until the Settings-toggle
  wiring lands, the app's only radio controls are quit, `wireless: false` at next connect, and the
  automatic off-on-app-loss.
  Source: `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` §3. Verified open 2026-08-16:
  `grep -rn "sendRadio" host/CarPlayHost host/CarlinkAndroid tools` → exactly one hit, the definition
  in `OCBMClient.swift`.

- **`FileLogger` has no in-session size or rotation cap.** Only an age-based sweep exists (delete
  files older than 14 days at startup), so one long session can grow a file unbounded.
  Source: `docs/ops/05_AUDITS.md` §"FIXES APPLIED — DEFERRED". Verified open 2026-08-16:
  `grep -n "prune\|fileSize\|truncate\|maxFile" host/CarPlayHost/carlink_macOS/App/FileLogger.swift`
  → only `pruneOldLogs()`.

- **Metadata seam deltas hop to main on a fresh unstructured `Task` per chunk.** Two chunks completing
  concurrently have no ordering guarantee, so deltas can apply out of order.
  Source: `docs/ops/05_AUDITS.md` §LOW. Verified open 2026-08-16:
  `grep -n "Task { @MainActor" host/CarPlayHost/carlink_macOS/App/MetadataWindow.swift` → one
  unstructured task, spawned per parse pass after `seamLock.unlock()` with no ordering primitive.

- **Media-port coordination — the app still cannot choose the media ports.** The box pre-binds and its
  own local response doubles as the oracle; the host copies `dataPort`/`controlPort` verbatim and
  passes non-authorable types straight through. This is the one genuinely-open clause left in docs/ops/04_OPEN_ITEMS.md's
  "Open design items".
  Source: `docs/ops/04_OPEN_ITEMS.md` §"Open design items", `docs/carplay/01_OCBM_PROTOCOL.md` §"RTSP channel". Verified open 2026-08-16:
  `grep -n "Pre-bind + oracle" -A4 crates/vendor/receiver/src/relay.rs`;
  `grep -n "copyPort" host/CarPlayHost/carlink_macOS/OCBM/AirPlaySetupSession.swift`.

- **No NACK retransmit.** Recovery is keyframe-request-on-seq-gap only; there is no bidirectional NACK
  channel on either end.
  Source: `docs/host/00_MACOS_HOST_APP.md` Tier 4. Verified open 2026-08-16:
  `grep -rni "nack\|retransmit" --include="*.rs" --include="*.swift" .` → every hit is iAP2
  link-layer; zero in any A/V or OCBM path.

- **Decoder color attachments (709 / 601-4 / sRGB) are not set.** Color is left to VideoToolbox
  inference; the only attachment touched is the per-sample NotSync flag.
  Source: `docs/host/00_MACOS_HOST_APP.md` Tier 4 ("cheap insurance"). Verified open 2026-08-16:
  `grep -rn "ColorPrimaries\|TransferFunction\|YCbCrMatrix" host/CarPlayHost/carlink_macOS --include="*.swift"`
  → zero hits.

- **No rate-matched ring buffer for long-session A/V clock drift; the behaviour on drift is to DROP.**
  The audit said "add one if it appears"; the audio player's own comment names clock drift as the
  reason it caps and drops.
  Source: `docs/host/00_MACOS_HOST_APP.md` Tier 4 (conditional). Verified open 2026-08-16:
  `grep -rni "drift\|ringBuffer\|rateMatch" host/CarPlayHost/carlink_macOS --include="*.swift"` → one
  hit, `AudioPlayer.swift`'s drop comment.

- **`OCBMAudioStreamFormat.bits` is still a write-only field.** Parsed from `SEAM_FORMAT` and stored,
  never read. (Its siblings `videoCounter`/`altVideoCounter` from the same audit bullet are gone.)
  Source: `docs/ops/05_AUDITS.md` §"FIXES APPLIED — DEFERRED". Verified open 2026-08-16:
  `grep -rn "\.bits\b" host/CarPlayHost/` → no hits outside markdown.

- **The OCBM frame header's `seq` (offset 12, u32) is written by every endpoint and read by none.**
  Distinct from the `SEAM_MAGIC` per-video-frame `u64` seq, which *is* read. Still the dead-field
  footgun the audit flagged.
  Source: `docs/host/00_MACOS_HOST_APP.md` Tier 4 OCBM cleanup. Verified open 2026-08-16:
  `grep -rn "\.seq" --include="*.rs" crates/ocbm-proto ccpa/ocbmd host/ocbm-host | grep -v "self.seq\|seq: "`
  → one hit, a unit assertion.

- **The SOM/EOM single-frame contract is neither asserted on the host nor implemented as coalescing.**
  `OCBMReassembler` surfaces `flags` on `OCBMFrame` and validates nothing; no consumer reads the
  field, so a fragmented frame would be delivered silently truncated.
  Source: `docs/host/00_MACOS_HOST_APP.md` Tier 4 OCBM cleanup. Verified open 2026-08-16:
  `grep -rn "\.flags" host/CarPlayHost/carlink_macOS --include="*.swift"` → zero consumers.

---

### Tests, tooling & install

> **Also see [../ops/04_OPEN_ITEMS.md](../ops/04_OPEN_ITEMS.md) Phase 4**
> item 3 for `ocbm_install.sh --full` not installing `iap_role_switch`, and item 10 for the
> documentation coverage gap.

- **The Swift config emitter has zero automated coverage.** `tests/run_tests.sh` compiles
  `VehicleConfig.swift` but not `SettingsWindow.swift`, where the emitter actually lives — and
  app/box schema drift is the repeated failure mode on this workstream. docs/carplay/04_CAPABILITIES_AND_CONFIG.md calls this the
  highest-value open item on workstream C. A built, mutation-verified proof of concept is preserved
  in `scratchpad/`, but its golden string is stale.
  Source: `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` §3 row C. Verified open 2026-08-16:
  `grep -n "VehicleConfig.swift\|SettingsWindow" host/CarPlayHost/tests/run_tests.sh` → one hit;
  `grep -n "iapConfigYAML" host/CarPlayHost/carlink_macOS/App/SettingsWindow.swift` → the emitter is
  in the uncompiled file.

- **`EMITTED_KEYS` is an allowlist, not a contract — 36 keys pinned against 76 the host reads.**
  Renaming any of the 40 unpinned keys blanks a pane with the suite green.
  Source: `docs/ops/05_AUDITS.md` §6. Verified open 2026-08-16: the `const EMITTED_KEYS` table in
  `crates/vendor/iap2-core/src/metadata.rs` has **36** `("kind", "key")` rows, against
  `grep -oE '(str|int|dbl|num|bool)\("[a-zA-Z]+"\)' host/CarPlayHost/carlink_macOS/App/MetadataWindow.swift | grep -oE '"[a-zA-Z]+"' | sort -u | wc -l`
  → **76**.

- **`host_app_reads_every_emitted_key` still scans the whole Swift file unscoped.** The key half of
  the assertion is a bare whole-file `contains("\"key\"")`, so an unrelated literal anywhere in
  `MetadataWindow.swift` satisfies it.
  Source: `docs/ops/05_AUDITS.md` §6. Verified open 2026-08-16:
  `grep -rn "host_app_reads_every_emitted_key" --include="*.rs" .` → the body is
  `if !src.contains(&format!("\"{key}\""))` against the entire file.

- **`skip_list_narrows_the_declaration` is still tautological, and `file_setting()` has no test.** The
  skip lever itself now has genuine coverage via `active_with_skip`, but the named test is unchanged
  and the `/tmp/carplay_metadata` parser has one caller and zero tests.
  Source: `docs/ops/05_AUDITS.md` §6. Verified open 2026-08-16:
  `grep -rn "skip_list_narrows_the_declaration\|file_setting" --include="*.rs" crates` → the test is
  still an `Iterator::filter` assertion; `file_setting` has a definition and one caller, no test.

- **`stream.rs` key derivation is unpinned — no known-answer test for the HKDF salt, info labels or
  nonce layout.** Encrypt/decrypt are exact inverses tested only for determinism, so a typo in
  `"DataStream-Salt"` passes the whole suite and kills every session.
  Source: `docs/ops/05_AUDITS.md` §6. Verified open 2026-08-16:
  `grep -rn "salt" crates/vendor/receiver/src/stream.rs crates/vendor/receiver/tests/*.rs` → the
  literals appear only in `derive_stream_keys`, never in an assertion.

- **The cross-language ChaCha20 known-answer test was never written.** The other three legs of the
  planned OCBM suite shipped; the KAT that would pin the Swift decrypt against the Rust encrypt
  exists on neither side.
  Source: `docs/ops/05_AUDITS.md` §"Desk-side prep". Verified open 2026-08-16:
  `grep -rn -i "known_answer\|chacha" host/CarPlayHost/tests/main.swift` → no matches.

- **`ocbmd` is still built at `opt-level = "z"`; the "measure first" decision was never taken.**
  `chacha20`, `poly1305`, `chacha20poly1305`, `ocbm-proto` and `receiver` are speed-tuned; the daemon
  that runs the poll loop and the per-frame forward is not.
  Source: `docs/ops/05_AUDITS.md` §"Also deferred", `docs/wireless/01_BT_AND_RADIO.md` §3. Verified open 2026-08-16:
  `grep -n "profile.release.package" Cargo.toml` → five overrides, none for `ocbmd`.

- **No swiftlint config.** The optional config from the structural batch was never created.
  Source: `docs/ops/05_AUDITS.md` §"Structural". Verified open 2026-08-16:
  `find . -name '.swiftlint.yml' -o -name 'swiftlint.yml'` → no matches.

- **The `/info` diff of a live Simulator session against ours on the same phone has never been
  performed.** docs/carplay/03_SDK_GROUND_TRUTH.md §11 names it as the productive move after concluding no further accessory-side
  RE explains why iOS ignores well-formed commands. **Read with `R-49-1`:** the diff as an
  *investigation* survives; "then tune it box-side" does not — `/info` content is app-authored.
  Source: `docs/carplay/03_SDK_GROUND_TRUTH.md` §11, `R-49-1`. Verified open 2026-08-16:
  `grep -rn "diff a live Simulator" docs/*.md` → only docs/carplay/03_SDK_GROUND_TRUTH.md's own lines; no follow-up doc or capture.

- **`CARPLAY_EVENTS_LOG` is set by no spawn site, so docs/ops/02_TESTING.md's byte-level discriminator greps read 0
  on a healthy box.** Reviving them means setting it in the `airplayd` spawn env first.
  Source: `docs/ops/02_TESTING.md` §"The discriminator", `R-46-1`. Verified open 2026-08-16:
  `grep -rn "CARPLAY_EVENTS_LOG" --include="*.rs" --include="*.sh" . | grep -v docs/` → four hits,
  all readers; `grep -n "airplayd" tools/session_supervisor.sh` → the spawn env is
  `OCBM_FWD_ENC=1 $CM $LT $MB`.

- **`tools/ocbm_boot.sh` is 76 lines behind `ccpa/rootfs/script/ocbm_boot.sh`.** The rootfs copy
  carries the first-boot dead-man and the NCM failover watchdog; the `tools/` copy has neither. Not a
  deployment hazard today — the installer places the rootfs copy — but the stale duplicate remains.
  Source: `docs/carplay/02_SESSION_LIFECYCLE.md` §"Supporting layers" ("Reconcile toward the rootfs copy, never away from it").
  Verified open 2026-08-16: `wc -l ccpa/rootfs/script/ocbm_boot.sh tools/ocbm_boot.sh` → 110 vs 34.

- **`carplay-status.sh` — the reader for the `/tmp/carplay_state` verdict — is not installed by the
  current installer.** The supervisor writes the verdict and tells you to "Read it with
  carplay-status", but only the older `install_fhs.sh` subset places the script, so a box built the
  current way has the state file and no reader.
  Source: `docs/carplay/02_SESSION_LIFECYCLE.md` §"Supporting layers" (Observability). Verified open 2026-08-16:
  `grep -rn "carplay-status" tools/*.sh` → `install_fhs.sh` and the supervisor's own message;
  `ocbm_install.sh`'s manifest does not list it.

- **The box network surface is unhardened — weak default PSK, unauthenticated `telnetd`, bare
  dropbear, no AP isolation, no iptables.** `radio_ap_up.sh` refuses to BLANK the passphrase but only
  writes one when the existing key is absent or under 8 chars, so an existing `12345678` is never
  replaced. **This is a deliberate development posture** (telnetd is the documented rollback channel)
  and therefore an owner decision, not a defect — recorded so it is a decision and not an oversight.
  Source: `docs/ops/05_AUDITS.md` §7 ("Outside the review's scope, for the owner"). Verified open 2026-08-16:
  `grep -n "wpa_passphrase" ccpa/rootfs/script/radio_ap_up.sh` → the guard is `[ "${#PSK}" -lt 8 ]`;
  `cat ccpa/rootfs/etc/init.d/rcS` → `busybox telnetd -l /bin/sh -p 23 &`;
  `grep -rn "ap_isolate" ccpa/` → no matches.

---

### Closed by decision — do NOT re-plan these

Recorded so a future session does not mistake an owner decision for unfinished work.

- **LogTransfer Tier-2 (serving the archive) will NOT be implemented** — owner directive 2026-08-07,
  no upside. Tier-1 advertise/negotiate is landed and device-proven. The byte-level unknowns
  (numeric `messageType`/`payloadType`, checksum algorithm and width, chunk-dict serialization,
  default/max `TransferChunkSize`) are recorded in `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` §2 should it ever be revisited.
- **Enhanced Siri is OUT OF SCOPE** — owner directive 2026-08-07; it needs a full hot-word and
  voice-analysis stack outside an A/V adapter's remit. The box stays advertise-inert and the app
  tooltip already says so (`docs/carplay/04_CAPABILITIES_AND_CONFIG.md` §6). Button-Siri works; the always-listening path does not exist.
- **The docs/carplay/03_SDK_GROUND_TRUTH.md §8 item 3 task "Emit `displayPanels[]` — the alt-content root cause" is REFUTED as a
  causal claim** (`R-49-2`, owner-confirmed on hardware 2026-08-11): cluster content works and its
  elements are toggleable via `showUI` query parameters. The *emission* remains unimplemented and is
  listed above under the capability surface — but as a schema-completeness item, not as the cluster
  fix docs/carplay/03_SDK_GROUND_TRUTH.md §8 still presents it as.
- **Phase 1 of [../ops/04_OPEN_ITEMS.md](../ops/04_OPEN_ITEMS.md) is
  closed in full** by owner decision (device identifiers and the vendor firmware backups). Nothing
  from it belongs in this index.

---

### Found closed — documents still call these open

Every entry here was listed as open, planned or deferred by a source document (or by a status banner)
and is shipped in the current tree. Listing them is the point: a stale "open" is exactly the false
claim this index exists to remove.

**From `docs/ops/05_AUDITS.md` §DEFERRED and §"DEAD CODE inventory":**

- **MicCapture re-wired, not retired.** `AppDelegate` instantiates it and pipes `onPCMData` to
  `client.sendMicPCM`, started and stopped from `onUplinkGate`.
  Verified 2026-08-16: `grep -rn "MicCapture\|onPCMData" host/CarPlayHost/carlink_macOS`.
- **NowPlayingManager and CallManager retired by deletion** (which also closes M-h).
  Verified: `find host -name 'NowPlayingManager*' -o -name 'CallManager*'` → no matches.
- **The legacy `AdapterProtocol` subsystem is gone**, with `TouchAction`/`CommandID`/the LE helpers
  extracted to `InputTypes.swift` exactly as docs/ops/05_AUDITS.md §Structural planned.
  Verified: `find host -type d -name Protocol` → nothing.
- **`ProtocolSessionRecorder` closed by deletion**, not by arming.
  Verified: `find host -name 'ProtocolSessionRecorder*'` → no matches.
- **The staged box deploy of the video fix happened** and was hardware-validated 2026-08-01.
  Verified: `grep -n "struct OutQueue\|out_video" ccpa/ocbmd/src/main.rs`.
- **M-k stereo PCM uplink RTP timestamp fixed** — the advance is per-channel sample-frames on both
  codec arms. Verified: `grep -n "SAMPLE-FRAMES\|per_ch" crates/vendor/receiver/src/uplink.rs`.
- **Three ocbmd LOWs fixed** — blocking `forward_input`, the 30 s `CT_SRC` srcbench, and the CH_IP
  `conns` leak across sessions. Verified: `grep -n "fn forward_input" -A20 ccpa/ocbmd/src/main.rs`
  and the `conns.clear()` calls on both teardown paths.
- **`net.rs` no longer tears down after one pre-A/V read timeout** — the `WouldBlock` arm consults an
  A/V-activity backstop. Verified:
  `grep -n "av_idle_ms\|AV_IDLE_TEARDOWN_MS" crates/vendor/receiver/src/net.rs`.
- **`AltVideoStream.max_fps` and `HidConfig.knob_support` are applied, not dropped**, and the dead
  `MBTN_*`/`ACODEC_*`/`ATYPE_*`/`sevHostGone`/`videoCounter`/`etaEpoch` symbols are swept.
  Verified: `grep -n "alt_max_fps" crates/vendor/receiver/src/vehicle_config.rs`;
  `grep -rn "MBTN_\|ACODEC_\|ATYPE_" crates/ocbm-proto/src/lib.rs` → none.
- **Communications 0x4158 and List 0x4171 are no longer structurally unreachable** — both are
  Extended-tier features in the generated table and `message.rs` unions `RCV_MSG_IDS` with
  `features::received_ids`. Verified: `grep -n "communications\|call_history" crates/vendor/iap2-core/src/features.rs`.

**From `docs/ops/05_AUDITS.md` (its Phase-2 banner is the source of most of these):**

- **eld-codec `AACENC_SBR_MODE=0` landed** — and the root cause was broader than the doc knew (the
  bitrate was also wrong). Verified: `grep -n "AACENC_SBR_MODE" crates/vendor/eld-codec/csrc/eld_shim.c`.
- **The desk-side batch shipped verbatim** — `frame_into`/`try_frame_into`, the `OutQueue` cursor,
  `enum SendOutcome { sent, droppedNotSubscribed, writeFailed }`, and the
  `OCBMSessionCoordinator.swift` split. Verified: `grep -n "fn frame_into" crates/ocbm-proto/src/lib.rs`;
  `grep -rn "SendOutcome\|onSubscriptionState" host/CarPlayHost`.
- **C1 timeout shrink + C4 blind-retry removal landed**, tagged in-source by audit id.
  Verified: `grep -n "C1\|C4" host/CarPlayHost/carlink_macOS/USB/USBTransport.swift`.
- **V1 AVCC fast path + V4 backpressure landed** as `Video/AVCCFastPath.swift`.
  Verified: `grep -rn "V1\|V4" host/CarPlayHost/carlink_macOS/Video/`.
- **W1 RFCOMM reassembly landed** — a persistent accumulator with a bounded desync drop.
  Verified: `grep -n "reassembl" crates/vendor/wireless/src/bt_driver.rs`.
- **The W-set landed** — ordered thread joins before A/V teardown, and the AV latch vouches for
  rx-connect by tracked pid. Verified: `grep -n "AV_RX_CONNECT_PID" crates/vendor/wireless/src/av.rs`.
- **The TSan scheme is enabled.** Verified: `grep -rn "enableThreadSanitizer" host/CarPlayHost/` → two
  `YES` entries in the shared xcscheme.
- **Finding B's direction 3 landed** (reap `rx-connect` by both name forms) — docs/ops/05_AUDITS.md's own inline
  note says "only fix direction 3 is unlanded", which is off by one; direction 2 is the open one.
  Verified: `grep -n "pkill" crates/vendor/wireless/src/av.rs`.

**From `docs/carplay/02_SESSION_LIFECYCLE.md` / `docs/carplay/06_AV_PIPELINE.md` / `docs/host/00_MACOS_HOST_APP.md` / `docs/ops/04_OPEN_ITEMS.md` (their banners under-report):**

- **Partial-stream teardown SHIPPED** — `AvSession::teardown` branches on stream count exactly as
  `AirPlayReceiverSession.c` does, with the empty-array-is-a-FULL-teardown correction applied. Both
  `docs/carplay/02_SESSION_LIFECYCLE.md` and `R-11-1` list it under "P2 — NONE SHIPPED".
  Verified: `grep -n "Partial teardown\|outDone=false" crates/vendor/receiver/src/session.rs`.
- **The control-channel inactivity watchdog gated on A/V-active flags SHIPPED** — neither the doc nor
  the ledger records it either way. Verified: `grep -rn "av_idle_ms" --include="*.rs" .`.
- **The 24 kHz mic rate SHIPPED on both ends** — the box negotiates AAC-ELD and OPUS 24 k mono and the
  host takes the box-negotiated rate rather than hardcoding 16 k.
  Verified: `grep -rn "24000" --include="*.rs" crates/vendor/receiver ccpa/airplayd`;
  `grep -rn "startCapture(" host/CarPlayHost/carlink_macOS --include="*.swift"`.
- **Backpressure-not-drop is in the tree**, not "BUILT … staged" — per-stream out-queues, video gated
  on its own backlog, audio never gated. Verified: `grep -n -i "backpressure\|never gated" ccpa/ocbmd/src/main.rs`.
- **The knob/telephony descriptors, `lane_guidance` and param 30 all shipped**, so `R-06-1`'s
  remaining-work list is itself partly stale. Verified:
  `grep -n "fn knob_descriptor\|fn telephony_descriptor" crates/vendor/receiver/src/info.rs`;
  `grep -rn "lane_guidance" crates/vendor/iap2-core/src/features.rs`;
  `grep -n "ident_info_wireless_transport_declares_route_guidance_param30" crates/vendor/iap2-core/src/message.rs`.
- **AltVideo shipped in full** — `CH_ALT_VIDEO 0x24`, seam `:9005`, dedicated host decoder and window
  — although `docs/host/00_MACOS_HOST_APP.md` Tier 4 still bundles it into one open bullet with VDC and NMEA GPS.
  Verified: `grep -rn "CH_ALT_VIDEO" crates/ ccpa/ host/CarPlayHost`.
- **docs/ops/04_OPEN_ITEMS.md's other two "Open design items" are genuinely resolved** — the `receiver_core` rootfs
  footprint (≈3.8 MiB measured) and the SETUP-relay latency (p99 2.36 ms, GO). Neither carries a
  surviving "Still open" clause; only media-port coordination does.

**From `docs/carplay/03_SDK_GROUND_TRUTH.md` / `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` / `docs/carplay/05_METADATA_AND_CONTROLS.md` / `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` / `../wireless/00_WIRELESS_CARPLAY.md`:**

- **`enablesUIAppearance` / `enablesMapAppearance` ARE parsed**, and `uiAppearanceUpdate`,
  `mapAppearanceUpdate` and `changeMapZoomLevel` all exist — docs/carplay/03_SDK_GROUND_TRUTH.md §5's body text still calls them
  absent. Verified: `grep -n "enablesMapAppearance" crates/vendor/receiver/src/vehicle_config.rs`;
  `grep -n "changeMapZoomLevel\|uiAppearanceUpdate" crates/vendor/receiver/src/events.rs`.
- **`knob_support` is wired end to end** (docs/carplay/03_SDK_GROUND_TRUTH.md §8 item 6 already annotates this STALE; confirmed so
  the index does not re-open it). Verified: `grep -rn "set_knob_advertised" --include="*.rs" crates ccpa`.
- **focusTransfer's lever and per-view `/info` flag DID land** — docs/carplay/04_CAPABILITIES_AND_CONFIG.md's "no `focustransfer()`
  lever exists / hard-false" is refuted. Only the SETUP echo is still missing (listed above).
  Verified: `grep -n "focus_transfer" crates/vendor/receiver/src/{info,levers}.rs`.
- **docs/carplay/04_CAPABILITIES_AND_CONFIG.md's "Current status" for Mechanism B is overtaken** — Identify param 20 no longer
  hardcodes `EngineType=Gasoline` and no longer omits `SupportedChargingConnectors` /
  `PowerForConnectorType`, and param 21 is emitted when the app pushes a `vehicleStatus:` block. Only
  "features.rs declares nothing" survives (listed above as C-4). Verified:
  `grep -n "engine_types\|SUPPORTED_CHARGING_CONNECTORS\|status_caps" crates/vendor/iap2-core/src/message.rs`.
- **docs/carplay/05_METADATA_AND_CONTROLS.md §5 item 5 shipped on both halves** — `viewAreas` IS echoed in the SETUP feature list, and
  `extendedFeatures` was hoisted out of the HEVC block. Verified:
  `grep -n "viewAreas" crates/vendor/receiver/src/session.rs`;
  `grep -n "extendedFeatures" crates/vendor/receiver/src/info.rs`.
- **docs/carplay/04_CAPABILITIES_AND_CONFIG.md's "a real fix would have the app retry HELLO until HELLO_ACK" shipped**, as did the
  "durable hardening" loss-tolerant decrypt counter (the box now stamps a per-frame `seq`).
  Verified: `grep -n "helloAcked\|HELLO_ACK" host/CarPlayHost/carlink_macOS/OCBM/OCBMClient.swift`.
- **The wireless research's "Still open" items 1 and 3 are closed** — the Bonjour responder on `wlan0`
  ships in `rx-connect`, and the link-key store reuses `carplay_peers.bin`. Verified:
  `grep -n "_airplay._tcp" crates/vendor/rx-connect/src/main.rs`;
  `grep -rn "carplay_peers" --include="*.rs" crates ccpa`.
- **docs/wireless/01_BT_AND_RADIO.md's radio re-gating is CLOSED** (`R-51-3`) — the guard sits on `wireless_up` itself, and the
  two call-site strategies that failed must not be retried.
- **docs/ops/00_BUILD_AND_DEPLOY.md's "`host/CarlinkAndroid` … not yet merged to `main`" is stale** — it is on `main`.
  Verified: `git ls-tree -d --name-only main host/`.

---

### False claims found while verifying

Beyond the "found closed" list, three documents assert something the code or our own captures
contradict. Recorded here, not fixed — this index changes no other document.

- **docs/carplay/05_METADATA_AND_CONTROLS.md's banner says "the host-side pane defect is still open". It is not a defect at all.**
  `docs/ops/05_AUDITS.md` §1 (four days later) diagnosed the empty Telephony/Power/Device panes as a **stale
  binary**: the session log began before either Release build existed, and a string probe confirmed
  `batteryChargeLevel`, `wirelessCarPlayAvailable` and `battery.100.bolt` were absent from the
  binaries that existed at that moment. The real residue is the nine unread `communications`
  capability keys, listed above. Verified 2026-08-16:
  `grep -n "stale binary" ../ops/05_AUDITS.md` → §1's heading and its
  `LogStoreManifest.plist` build-timestamp evidence.

- **docs/carplay/03_SDK_GROUND_TRUTH.md §8.1 — and the `message.rs` comment repeating it verbatim — assert that "iOS can never
  send `0x5204`" without param 30 sub 8. Our own capture says otherwise.** `docs/ops/05_AUDITS.md` §9 records the
  2026-07-29 session delivering `0x5204 LaneGuidanceInformation` ×12 alongside `0x5201` ×574, as the
  first live confirmation that a subscribe-less `Trigger::RidesOn` feature receives data. The param-30
  expansion is still genuinely open; the *reason* stated for it is empirically wrong.
  Verified 2026-08-16: `grep -n "0x5204" ../ops/05_AUDITS.md ../carplay/03_SDK_GROUND_TRUTH.md`;
  `grep -n "iOS can never send" crates/vendor/iap2-core/src/message.rs`.

- **docs/ops/05_AUDITS.md's Phase-2 banner over-closes.** It reads "⚠️ EVERYTHING IN THIS SECTION SHIPPED — do not
  treat any item below as outstanding work", but `R-50-2`'s evidence covers only the desk-side batch
  and the `Protocol/` deletion. At least six items in that same section have not shipped: I2 link
  dedupe, W9 BT MAC, `CT_INPUT_NACK`, `connect_seam` numeric-only, ocbmd `opt-level=2`, and the
  cross-language ChaCha20 KAT — all six are listed above. Separately, docs/ops/05_AUDITS.md's Finding-B inline note
  ("only fix direction 3 is unlanded") is off by one: direction 3 landed, direction 2 did not.
  Verified 2026-08-16: the six `Verified open` commands in the sections above.

Two further items are open with a **wrong stated cause**, worth flagging so they are not re-planned as
the wrong work:

- **docs/host/00_MACOS_HOST_APP.md Tier 1 #5 blames the box** ("the box only advertises a single-touch HID descriptor"). The
  box ships Apple's two-finger descriptor with a unit test against Apple's fill order; the gap is
  entirely host-side. The same stale claim appears as an in-code comment beside
  `didMultiTouchTwo`. Verified 2026-08-16:
  `grep -n "touchscreen_multi_descriptor\|multi_touch()" crates/vendor/receiver/src/info.rs`.
- **`crates/vendor/eld-codec/csrc/eld_shim.c`'s opening comment still narrates `AACENC_SBR_MODE` as
  "left at -1 (auto)"** eighteen lines above the call that pins it to 0. Historical narration of the
  bug, contradicted by the code below it.

---

### Unverified

Listed with exactly what was tried. None of these can be settled by reading the tree.

- **`R4` — "pre-auth gate (Apple 470)"** (`docs/ops/05_AUDITS.md` §"Hardware session 1"). docs/ops/05_AUDITS.md names the item only
  by tag, the underlying fix proposal is in no document in the tree, and no code carries the marker.
  Tried: `grep -rn "470" --include="*.rs" crates ccpa` → nothing; `grep -rn "pre-auth\|preauth" docs/*.md`
  → only docs/ops/05_AUDITS.md's own line. The one in-code "audit R4" tag (`session.rs`, a `/tmp` size cap) is a
  different finding, so the Phase-2 R-numbering does not map onto the in-code audit tags.
- **`R2` — SESSION-lock chip-op restructure** (`docs/ops/05_AUDITS.md` §"Hardware session 2"). The MFi lock
  discipline in `mfi_local.rs` looks sound (bounded non-blocking acquire, held across the whole
  stateful sign sequence), but nothing establishes whether that IS the restructure or the state it
  wanted changed. Tried: `grep -n "flock\|MfiLock::acquire" crates/vendor/wireless/src/mfi_local.rs`;
  the in-code "audit R2" tag is in `events.rs` and is a different finding.
- **`U9` / `U13b` input changes** (`docs/ops/05_AUDITS.md` §"Hardware session 3"). Named only by tag, with no
  description anywhere in the tree. Tried: `grep -rn "U9\|U13" host/CarPlayHost/ ../ops/05_AUDITS.md`
  → one hit, docs/ops/05_AUDITS.md's own line. Contrast V1/V4 and C1/C4, which ARE tagged in-source.
- **Hardware-observation gaps that no grep can settle.** The ACL-drop outcome (plan_A test 3), the
  remaining app/phone checklist steps (plan_A §5 tests 2–8 and 10), the app-pushed metadata tier
  ("landed 2026-08-10, NOT yet hardware-validated"), and `docs/carplay/06_AV_PIPELINE.md`'s ○-marked audio formats
  (advertisable, decode not device-confirmed). Tried: nothing beyond reading the documents — settling
  any of them needs the box and a phone.
- **`docs/carplay/02_SESSION_LIFECYCLE.md` §7 items 2–4 — three open protocol questions.** Whether `disableBluetooth` arrives before
  or after session-start on a real device; the literal bytes CINEMO puts in its first tunnelled
  message; and whether iOS honours a subscribe for an id the *tunnel* Identify declared but the *BT*
  Identify did not. Each needs a device capture or vendor observation, not a code read. Tried:
  `grep -rn "disableBluetooth" --include="*.rs" crates ccpa` → the handler exists, but nothing records
  its observed arrival order.

---

## Roadmap

<!-- absorbed: ../ops/04_OPEN_ITEMS.md -->

### Proven (on hardware)

- **OCBM transport** — `/dev/usb_accessory` bulk (VID `0x1314` PID `0x2d00`, IF0, EP IN `0x81` / OUT
  `0x01`, 512 MPS, USB 2.0 HS) *(**CORRECTED 2026-08-31** — this said `0x83`/`0x02`, which is what
  the gadget enumerated on an earlier build. docs/carplay/00_ARCHITECTURE.md §"Endpoint-address correction" is the authority
  and this line contradicted both it and §GM-AAOS further down THIS file. Do not hard-code either
  pair: enumerate. Live confirmation, every OCBM session this month: `IF0 bulk IN=0x81 OUT=0x01`.)*, **339 Mbps down / 90 Mbps up** *(**CORRECTED 2026-08-16** — this said
  "~335 / 222 up"; docs/carplay/00_ARCHITECTURE.md's measured table is the authority, and it explains the asymmetry as an
  i.MX6UL `acc_read` limit. The 220 Mbps figure elsewhere is a round-trip echo push, not a raw OUT
  rate)*, non-blocking priority scheduler. Channels
  live: CTRL `0x00` (incl. session-control SUBSCRIBE/heartbeat/presence), MFI `0x01` (chip service),
  CONSOLE `0x02` (root PTY), IP `0x10` (TCP/UDP mux), **FILE** `0x11` (verified binary deploy —
  `ocbm-host push`, md5-matched), **ETH** `0x12` (raw-frame bridge — diagnostic),
  **VIDEO/MEDIA_AUDIO** `0x20`/`0x21` (encrypted A/V forward), **ALT_AUDIO** `0x22` (voice sink,
  seam `:9003`), **METADATA** `0x23` (seam `:9004`), **ALT_VIDEO** `0x24` (cluster screen, seam
  `:9005`), **INPUT** `0x30` (HID uplink), **MIC** `0x31` (mic uplink), **MGMT** `0x40` (box
  management / the app's "CCPA" tab), **RTSP** `0x41` (app-driven SETUP relay, seam `:9106`), ECHO
  `0xFF`, DISCARD `0x0FFF`. `CT_SETTIME` clock-sync verified. Authoritative constants:
  `crates/ocbm-proto/src/lib.rs`.
- **Root + deploy** — open telnet/SSH over USB-NCM (no password set as shipped; no keys or certificates), UART console (persistent 115200), SPI programmer, live
  overlay iteration; plus OCBM `push` for binaries once `ocbmd` is running. **Persistent FHS install**
  (daemons in `/usr/sbin`, tools in `/usr/bin`, scripts in `/script`) + turnkey boot: a cold boot comes
  up with a live OCBM link, zero bootstrapping (reboot-proven). **CORRECTED 2026-08-16 — the installer:**
  the current path is `tools/ncm_base_install.sh` then `tools/ocbm_install.sh --full` (reversible trial
  before finalize; `--full` also places `carplay-wireless` and the `radio_detect.sh`/`radio_hal.sh`/
  `radio_ap_up.sh` seam). `tools/install_fhs.sh` still works but is the older OCBM-era subset —
  `ocbmd`/`iap2d`/`airplayd`/`rx-connect` + `iap_role_switch` only, no wireless stack and no radio seam.
  See [`../ops/01_RECOVERY.md`](../ops/01_RECOVERY.md).
- **MFi** — genuine 2.0C coprocessor at `/dev/i2c-1 @0x11`, driven from userspace (`iap2d`).
- **Phone-side iAP2 handshake — reaches Identified** against a real iPhone (SYN-ACK → cert/0xAA01 →
  sign/0xAA03 → 0xAA05 → 0x1D01 → 0x1D02), and holds the link. Unblocked by the `link.rs::parse`
  coalesced-read fix.
- **Adapter pairing + key derivation** — `ccpa/airplayd` on `ncm0` completes pair-setup/pair-verify +
  MFi-SAP (local chip) and derives the ChaCha20 session key against a real iPhone. **Disk-backed
  PeerStore** (`/etc/carplay_peers.bin`) persists pairing — a known device reconnects pair-verify-only.
- **Forward-encrypted A/V** — the box forwards the **encrypted** video + audio + hands the per-stream
  key over OCBM (`CH_VIDEO`/`CH_MEDIA_AUDIO`); a Rust debug receiver (`ocbm-host avdec`) decrypted
  host-side: hundreds of video frames + thousands of audio packets, **0 decrypt failures**.
- **Session lifecycle** — host-app-driven (SUBSCRIBE → IDLE→projection → ARM; STOP/crash → TEARDOWN →
  holding pattern), hardware-validated across a reboot. Full spec: [`../carplay/02_SESSION_LIFECYCLE.md`](../carplay/02_SESSION_LIFECYCLE.md).
- **Foundation-hardening pass (2026-07-10)** — a 12-agent code analysis verified our crypto/protocol implementation against Apple's `CarPlaySDK` reference behavior and the published specification (confirmed byte-for-byte correct); the robustness
  findings it raised were addressed in follow-up work.
- **CH_ETH bridge** proved the iPhone's live link-local (mDNS/NDP) traverses OCBM to the Mac — kept as
  a diagnostic (not the A/V path in the committed model).
- **Receiver** — `ncm_carplayd/receiver_core` reached a sustained genuine CarPlay session (iOS 27),
  HEVC confirmed; reused (its pairing/session path now runs on the box).

### Building blocks (committed order)

1. ~~OCBM v1 framed multiplexer + channels~~ **DONE.**
2. ~~Phone-side iAP2 handshake to Identified~~ **DONE.**
3. ~~**Adapter pairing path.**~~ **DONE.** `ccpa/airplayd` reuses receiver_core's `ControlServer`
   (pair-setup/verify + `/info`) + `pairing`/`rtsp`/`mfi`, backed by a `LocalMfiSigner` on the local i2c
   chip. Full pair-setup → pair-verify → MFi-SAP → ChaCha20 session key ran end-to-end against a real
   iPhone; mDNS `_airplay._tcp` on `ncm0` via `rx-connect`. **Disk-backed PeerStore**
   (`/etc/carplay_peers.bin`) persists pairing — known device reconnects pair-verify-only.
4. ~~**Session key + encrypted A/V over OCBM.**~~ **DONE.** The box forwards the encrypted media
   untouched + hands the per-stream key over `CH_VIDEO`/`CH_MEDIA_AUDIO`; a Rust debug receiver
   (`ocbm-host avdec`) decrypted host-side (video + audio, 0 failures).
9. ~~**Session lifecycle & host-presence management.**~~ **DONE.** (**NOTE 2026-08-16** — this item and
   "Metadata" below are both numbered `9.` in the original list; the duplicate is left as written. Where
   the banner above says "item 9" it means **Metadata**.) Host-app-driven: SUBSCRIBE (CH_CTRL)
   → IDLE→projection (`projection_up.sh`) → ARM (`session_supervisor.sh` starts airplayd + rx-connect);
   STOP/crash → TEARDOWN → holding pattern (iap2d stays up). Live-UI = backpressure, not drop (per-stream queues,
   gated seam reads; see docs/carplay/02_SESSION_LIFECYCLE.md); ocbmd tracks presence via `/tmp/host_present`. Hardware-validated across a reboot.
   Full spec: [`../carplay/02_SESSION_LIFECYCLE.md`](../carplay/02_SESSION_LIFECYCLE.md).

**Roadmap items 1–10 below are all complete** (5/9/10 hardware-validated; item 8 wired 2026-08-08 and
wireless 2026-08-10). They are kept because the per-item notes record how each was validated; what is
still open is §4 of this file, not this list.

5. **Host app (THE critical path).** ✅ DONE 2026-08-01 (built + hardware-validated). Rebuild from `carlink_macOS` / `ncm_carplayd/macos` carplay-app:
   claim `1314:2d00`, SUBSCRIBE over CH_CTRL, decrypt with the handed-over key, **decode HEVC + render
   pixels** (VideoToolbox), play audio, and **send touch input back** (the input uplink — CH_INPUT,
   ✅ wired: `CarPlayView` touch → `OCBMClient.sendTouch` → CH_INPUT; ~~not yet exercised~~, corrected
   2026-08-01 — box-side touch/media HID arrival confirmed 2026-07-12). This replaces the
   debug receiver and closes the loop to a usable head unit.
6. **Nav voice / mic.** ✅ **DONE — corrected 2026-08-10 (this line was stale).** Nav voice DOES have
   its channel: `forward.rs` tags every AU on `:9003` (`[rate:u32 BE][ch:u16 BE][atype:u8][len:u32 BE][AU]`) and
   `crates/vendor/receiver/src/session.rs` routes **by `audioType`, not stream type** — media → media
   sink (:9002), everything else (telephony/speechRecognition/alert/default) → the voice sink (:9003)
   (search `Route by audioType`; ~`:871`/`:880` as of 2026-08-16). The mic
   uplink exists too: host `MicCapture.swift` → OCBM → airplayd's `MIC_INGEST_ADDR 127.0.0.1:9112`.
   **Owner-confirmed on hardware 2026-08-10: nav prompts audible, Siri invocable and spoken to, phone
   calls working BOTH ways.**
   ~~Nav voice on `:9003` has no OCBM channel yet; plus the mic uplink for Siri.~~
   STILL OPEN, and it is a DIFFERENT feature: **Enhanced Siri** (`AuxOut` 106 / `AuxIn` 107) — the
   always-on mic path with ECNR, the in-car historical ring buffer and the two mandatory detectors
   (authority: `wwdc2019-252.txt:86-134`). `session.rs` records AuxOutAudio / AuxInAudio /
   MainBuffered as unimplemented and therefore omitted from SETUP responses (search "Still unimplemented
   and therefore omitted"; ~`:1516` as of 2026-08-16). Button-Siri works; the always-listening path does not exist.
7. **YAML config consumption.** ✅ **DONE — marked 2026-08-10** (it was already shipped; this line
   simply never got its marker, which made the doc contradict its own "items 1-10 complete" note).
   App-side truth (`VehicleConfig.swift` + `SettingsWindow`), pushed subset consumed by
   `vehicle_config.rs` and applied per control connection by `airplayd::load_device_config` — runtime
   feature flip, no recompile; `base_device_config()` is the app-less fallback template.
8. **Move SETUP app-side.** ✅ DONE 2026-08-08 (wired, hardware-validated; the
   `accessoryConfig.appDrivenSetup` toggle default was flipped **ON** 2026-08-09) — commits `84d2b80` (P0 RTT harness + P1 box CH_RTSP relay
   + transparency proof), `692cc80` (P2 Rust-harness authoring + live oracle), `89c457b` (P3 Swift
   port). (**CORRECTED 2026-08-16:** the three hashes previously printed here — `c5af75f`, `b09aee7`,
   `3d56e28` — exist in no ref.) The host app
   drives the post-pairing SETUP/RECORD negotiation over the OCBM `CH_RTSP` relay; the box's local
   response is the always-available fallback. Latency gate PASSED (docs/carplay/02_SESSION_LIFECYCLE.md: p99 2.36 ms under A/V load,
   ~21× inside the 50 ms bar). Go/no-go recorded in **docs/carplay/02_SESSION_LIFECYCLE.md**. Box-driven SETUP (`receiver::session`)
   is now the selectable fallback — app-driven SETUP is the default on BOTH transports (wired flipped
   ON 2026-08-09, wireless 2026-08-10 by owner directive; the docs/carplay/04_CAPABILITIES_AND_CONFIG.md earned-fallback pattern done
   right). The selection gate is `appsetup() && seam_up()` — the `!wireless` term is gone. The
   wireless flip required a phase-1 feature-token union (the host preserves the box-only
   `iAPChannel`/`sessionManagement`); see docs/carplay/04_CAPABILITIES_AND_CONFIG.md for why stripping `iAPChannel` would have killed
   the iAP2 tunnel. **Remaining follow-up:** migrate those two tokens to per-transport app config
   (workstream C/D debt, recorded in docs/carplay/04_CAPABILITIES_AND_CONFIG.md).
   Current pick-up: ../ops/04_OPEN_ITEMS.md. This
   realizes the future-proofing target of README §7 (SDK-evolving SETUP logic app-side).
9. **Metadata** (NowPlaying / Nav) — ✅ DONE (surfaced via `features.rs`-generated params 6/7 + subscribes; see docs/carplay/05_METADATA_AND_CONTROLS.md). ~~not yet surfaced; likely needs a subscribe/command or a channel `AvSession` doesn't currently expose.~~
10. **Wireless CarPlay** (BT-RFCOMM iAP2 + WiFi SoftAP + credential handover). ✅ DONE for transport,
    pairing and A/V (see docs/carplay/05_METADATA_AND_CONTROLS.md). ~~**⚠️ The METADATA plane regressed 2026-07-31 → 2026-08-10** (a
    scid guard rejected the type-130 DataStream SETUP; tunnel stuck at `Init`, zero NowPlaying). Fix
    landed 2026-08-10, **not yet hardware-validated** — do not re-mark this line fully DONE until a
    soak shows `0x5001` records arriving.~~ **RESOLVED — the validation gate this line set was met on
    2026-08-11**: Identify accepted, `decryptFail=0`, NowPlaying and route/lane guidance arriving
    (README §Status, 2026-08-11). Regression evidence retained:
    `docs/ops/captures/2026-08-10_REGRESSION_datastream130_scid_rejected.txt`.
    ~~Rootfs radio scripts exist; handshake documented in `CPC200-CCPA_resources`. Later phase.~~

### Open design items

- ~~OCBM media framing: how encrypted stream payloads + the key handoff are carried.~~ **RESOLVED** —
  per-stream channels each carry a key handoff then raw encrypted frames; validated 0-failure host-side
  decrypt. (The video lanes additionally carry `SEAM_MAGIC` + a `u64 LE seq`, and the key is re-handed on
  every seam reconnect — see docs/carplay/01_OCBM_PROTOCOL.md §"Media transport" for the byte-exact framing.)
  ~~Still open: minimal RTSP framing the box relays vs the app parses (the SETUP box↔app boundary, incl.
  media-port coordination), once SETUP moves app-side (step 8).~~ **RESOLVED 2026-08-16 for the framing
  half** — it shipped with step 8: `CH_RTSP 0x0041` carrying the v1 `RS_OPEN`/`RS_REQ`/`RS_RESP`/
  `RS_CLOSE`/`RS_ERR` sub-frames (`crates/ocbm-proto/src/lib.rs`, constants authoritative in
  `receiver::relay`; wire spec in docs/carplay/01_OCBM_PROTOCOL.md §"RTSP channel (`0x0041`)"). **Still open: media-port
  coordination only** — the box pre-binds and its local response doubles as the oracle, so the app
  cannot yet choose the media ports (docs/carplay/01_OCBM_PROTOCOL.md §"RTSP channel" / §"Media transport").
- ~~`receiver_core` footprint on the 6 MB rootfs (crypto crates; run-from-tmp; size-opt/UPX) — measure
  the armv7-musl build (see [`../ops/00_BUILD_AND_DEPLOY.md`](../ops/00_BUILD_AND_DEPLOY.md)).~~ **RESOLVED** — measured
  2026-07-10 and re-measured 2026-08-16: the pairing/session path cross-compiles clean, and the whole
  shipped set is ≈**3.8 MiB unpacked** (`airplayd` ~1.71 MiB, `rx-connect` ~574 KiB, `iap2d` ~551 KiB,
  `carplay-wireless` ~537 KiB, `ocbmd` ~443 KiB), roughly half that UPX-3.96-packed — comfortably inside
  the ~6 MB free, and run-from-`/tmp` was never needed. Numbers and method:
  [`../ops/00_BUILD_AND_DEPLOY.md`](../ops/00_BUILD_AND_DEPLOY.md).
- ~~**SETUP relay latency** — driving SETUP app-side relays the control connection box↔app over USB;
  measure it doesn't destabilize the timing-sensitive pair/SETUP/RECORD phase.~~ **RESOLVED 2026-08-08
  (docs/carplay/02_SESSION_LIFECYCLE.md): PASS** — p99 2.36 ms under real A/V load (upper-bound path), ~21× inside the 50 ms gate,
  zero timeouts/cap-clears. Go/no-go in docs/carplay/02_SESSION_LIFECYCLE.md.

### Host-app / integration (not adapter firmware)

- **GM AAOS USB permission prompt: SOLVED and DEVICE-TESTED (2026-08-17).** The reappearing permission
  dialog on GM AAOS (`gminfo37`) is a head-unit bug — `framework-res`'s
  `config_UsbDeviceConnectionHandling_component` points at the stripped `android.car.usb.handler`, so
  the third-party implicit-grant path dead-ends (native CarPlay/AA/storage are unaffected; they route
  around it via system privilege). Fix, proven on a 2024 Silverado: ship the host app **UNDER package
  `android.car.usb.handler`, installed in user 10** → the framework grants USB permission to its UID
  **silently on every attach, no dialog, no root.** A controlled on-vehicle A/B (handler present → no
  dialog; handler removed → dialog returns) confirmed the squat is the sole cause. The grant is
  **per-UID**, so the claiming app must itself BE that package (a separate app still prompts; `MANAGE_USB`
  to re-grant needs priv-app / `/system` write the locked GM lacks — `adb pm grant` refuted). Landed in
  the `gm_ccpa` wireless receiver (`zeno.gmccpa` re-packaged to `android.car.usb.handler`, NoDisplay
  `UsbHostManagementActivity` trampoline). A standalone proof module also exists in
  `host/CarlinkAndroid/usbhandler`. Full analysis, evidence, refuted alternatives and the endpoint
  correction (`0x81/0x01`, not `0x83/0x02`):
  [`../host/01_ANDROID_AND_AAOS.md`](../host/01_ANDROID_AND_AAOS.md).
  Adapter side needs no change (the `0x1314:0x2d00` identity is already correct — docs/carplay/00_ARCHITECTURE.md).

### Contingencies (future — if/when measurements demand it)

- **Adapter-driven SETUP — ALREADY THE SHIPPED FALLBACK, not a future contingency** (corrected
  2026-08-10; it also cited the wrong step — app-driven SETUP is step **8**, step 6 is nav voice/mic).
  When `appsetup() && seam_up()` is false the box drives SETUP locally from the app-pushed config
  (the `else` arm of the `levers::appsetup() && receiver::relay::seam_up()` gate in
  `ccpa/airplayd/src/main.rs` — grep the condition, not a line number; that file moves often);
  `CARPLAY_APP_SETUP=0` forces it. Historical note: if app-driven SETUP (step 8) had added too much
  latency and risk session stability, fall back to having the **adapter drive the SETUP negotiation
  locally from the on-box YAML** (the config pushed down). The fallback stays bounded to rendering the
  app-pushed config — adapter-owned SETUP capability does not grow (docs/carplay/04_CAPABILITIES_AND_CONFIG.md directive 4). Keeps the negotiation next to the iPhone (no relay latency) at the cost of
  some future-proofing (the adapter would then carry SETUP logic that evolves with SDK versions). Only
  consider if the SETUP-relay-latency measurement above shows a real problem. See `README.md` §7.
- **Decode-on-adapter fallback** (deep contingency): the box could decode/re-emit typed A/V (the
  original CCPA model) — retained only as a last resort; the i.MX6UL has no VPU, so video is forwarded
  either way and this buys little.
