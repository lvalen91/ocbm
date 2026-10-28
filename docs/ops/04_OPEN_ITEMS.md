# Open items and roadmap

> **STATUS:** CURRENT · single owner for this topic. Consolidated 2026-08-31 from pre-consolidation docs OPEN, 06; the originals are in git history and in the 2026-08-31 backup. Correct this file in place — do not add a sibling.

Every known-open item in one place, plus the ordered roadmap. An item leaves this file only when it is closed with evidence.


## Settled — do not re-raise

- **`pi/evidence/hostapd_5g.conf` carries a `wpa_passphrase`, and that is accepted.** The value in
  it is a deliberate placeholder for a bench AP and protects nothing. (Corrected 2026-10-08: gm_ccpa's head-unit
  credential was NOT a placeholder; it is removed from source — OD-7A — and the owner chose no rotation/scrub.) Assessed and accepted by the owner 2026-08-31, explicitly including the case where either
  repo gains a remote. No rotation, no history scrub. Three separate audits have flagged the key
  *name* without reading the value — pattern-matching on `wpa_passphrase` is not a finding.
- **ccpa_custom history carries assistant-session trailers and conversation links from the gm_ccpa
  subtree import, until a planned local history rewrite removes them.** (Corrected 2026-10-28: this entry
  said there were none, verified 2026-08-31. `host/gm_ccpa` was imported as a subtree on 2026-09-08
  (`b049810`) and brought 36 commits dated 2026-08-09 to 2026-08-14 that carry both a session trailer and
  a conversation link; `git log --all` on main finds them. The former sibling gm_ccpa repo no longer
  exists as a separate tree.) Owner decision 2026-10-28: the owner removes them with a later local rewrite
  of ccpa_custom, which is not pushed anywhere. The publication mirror blocks them through its pre-push
  hook. Never push ccpa_custom itself. Write neither pattern out literally in a doc — the hook scans
  added CONTENT as well as commit messages, so even a sentence quoting them will block the push.
  (The rewrite does not by itself resolve the gm_ccpa credential question in the item above; that stays
  the owner's recorded decision.)

## Open items

<!-- absorbed: ../ops/04_OPEN_ITEMS.md -->

Open work in this project was once scattered across a 58-document flat corpus (consolidated
2026-08-31 into the five-category `docs/` tree). **This is the index for the CCPA / CarPlay / host-app
workstream — one line per item plus a back-pointer. It is not a spec and not a plan**: the detail, the
reasoning and the wire evidence stay where they already are, and nothing here supersedes, replaces or
closes anything.

**Other open lists, not duplicated here:** `baseline/README.md` section 7 (the mainline-kernel
baseline image: open items, measurements, verification); `pi/docs/00_PI_AAOS_PORT.md` section 6 and
`pi/docs/03_BRINGUP_RUNBOOK.md` section 5 ("What is NOT done", the Raspberry Pi / AAOS port); and, for
the C2Air (a different box: Allwinner V821, riscv32), `c2air/README.md` and `c2air/docs/`, plus the C2Air research
repo's `docs/kernel.md` section 9 (that repo is outside this tree and its path was not located on
2026-10-28, so the section is UNVERIFIED from here).

**Every item below was re-verified against the working tree on 2026-08-16 by the command shown on
its `Verified open` line, and re-audited against code and git on 2026-10-28.** The 2026-10-28 pass
closed or corrected the items marked so below (the corrections are inline and dated). The source
documents were up to five weeks old at the first pass and a large number of their "open" items had
shipped. Items the documents still called open but the code shows closed are listed under
[Found closed](#found-closed--documents-still-call-these-open) — that list is a deliverable, not a
failure. Items that could not be settled either way are under [Unverified](#unverified).

Anchors are **symbols and greppable strings, not line numbers** — line-number rot is this corpus's
single largest source of false claims. Grouping is by subsystem, because a reader wants "what is
open in the host app", not "what does docs/ops/05_AUDITS.md say". (The former "DO NOT" section and
"Phase 1-4" catalogue this paragraph and some items below pointed at no longer exist in this file.)

**Scope note.** Owner-decided WONTFIX items are recorded under
[Closed by decision](#closed-by-decision--do-not-re-plan-these) so they are not re-planned as work.
Superseded *design positions* (box-owned `/info` policy, the `/tmp/carplay_metadata` lever as a
canonical control, box-autonomous page-on-boot) are **not** open items and are not listed;
[06_CORRECTIONS_LEDGER.md](06_CORRECTIONS_LEDGER.md) carries those. Statements below about the box
(I2C MFi chip, jffs2 rootfs, supervisor) are about the CPC200-CCPA (NXP i.MX6UL, ARMv7) unless they say otherwise.

---

### Box daemons — `ocbmd` / `carplayd` / `iap2d` / the supervisor

> *(2026-10-28: this block pointed at a "Phase 4" catalogue of ten code defects that does not exist in
> this file. Of the three it named: the missing `iap_role_switch` install is CLOSED —
> `tools/ocbm_install.sh` manifest now lists `accessory_init/iap_role_switch.armv7` -> `/usr/bin/iap_role_switch`;
> the CT_HELLO `out_console` resync and `ccpa/rootfs/script/custom_init.sh` still calling
> `/script/start_bluetooth_wifi.sh` (a call that exists, `custom_init.sh`) were not re-evaluated and are
> UNVERIFIED. The other seven defects were never listed anywhere recoverable.)*

- **F4/F5 lever tranche — IN TREE 2026-10-19, NOT DEPLOYED, bench checks pending.** (F5) every
  env / flag-file lever is no-config-only: `/tmp/cornermask_test`, `/tmp/logtransfer_test`,
  `/tmp/mainbuffered_test`, `CARPLAY_APP_SETUP=1` (`receiver::levers::SubordinateLevers`) and
  `/tmp/carplay_viewarea2` (`DeviceConfig::host_configured`) — owner decisions D2/D3,
  docs/carplay/04_CAPABILITIES_AND_CONFIG.md §2. (F4) Android Auto is explicit opt-in on the box
  (D1, `box_common::cfg::aa_requested` / `aa_withdrawn`, supervisor `aa_enabled`, btd SDP record +
  channel-4 accept); the Android host now emits `android_auto: false` + `wifi_ap: true`, the macOS
  host emits `wifi_ap:` unconditionally. Close with the hardware checks (iPhone wired + wireless,
  Pixel wired AA, AA toggle off mid-session, ocbmd restart during AA, stale `/tmp/cornermask_test` /
  `/tmp/logtransfer_test` with the app pushing `false`, SDP browse with `android_auto: false`).
  Known limits left open: btd evaluates the AA SDP record when its SDP server starts, so turning AA
  ON with the radios already up needs a wireless restart before Android phones see the record (the
  channel-4 accept re-checks every connect regardless); a resident `aa-bridge --wireless` is not
  reaped when AA is turned off (it refuses every phone; wireless_down reaps it).
- **`wireless_up` held for wired Android Auto left wireless CarPlay down — FIXED IN TREE 2026-10-19
  (extra to F4/F5), bench check pending.** `wireless_up` returned at its AA choke point without
  recording anything, and every caller is an edge, so unplugging the Android phone left wireless
  CarPlay down until the app reconnected. It now sets `_wl_held_for_aa`, and the supervisor main
  loop re-runs `wireless_up` once the hold has been clear for `WL_AA_RETRY_TICKS` consecutive
  ticks and no `wireless_rebring_at` settle is pending (log: `wired Android Auto hold clear for 15
  ticks -> retrying the suppressed wireless bring-up`). *(Corrected 2026-10-19, review: the first cut
  used 5 ticks, which a slow AOAP switch defeats — the phone is off the bus and no owner flag is set
  for up to aa-bridge's 12 s `wait_for_accessory`; the constant is now `AA_ACCESSORY_WAIT_S + 3` = 15,
  so recovery after a real unplug takes ~15 s. The first cut also ignored a pending post-restart
  settle and could race wireless_down's detached rmmod.)* NOT fixed, and UNVERIFIED (code read only): the sibling case looks equally unrecorded —
  a host-present edge that arrives while a WIRED iPhone is already plugged (Hot-Handover off) is
  suppressed by the same function, A2's rising edge then finds no stack to tear down and so never
  sets `radios_yielded_to_wired`, and the falling edge restores nothing after the unplug. A2's own
  comment says the restore claim is conditional on purpose, so this needs an owner call, not a patch.
- **Host-app controls waiting on box verbs (2026-09-25, from the Android Settings → OCBM audit;
  app side landed in `host/CarlinkAndroid`, see OCBMANDROID.md "Settings → OCBM action mappings").**
  The app hooks exist and are capability-gated; NOTHING below is implemented box-side, and no verb
  id is assigned — the proposed shapes are in
  [../carplay/01_OCBM_PROTOCOL.md](../carplay/01_OCBM_PROTOCOL.md) ("Proposed MGMT verbs").
  1. **`MGMT_DISCONNECT_PHONE`, transport-aware, plus a capability bit.** Problem: the app's
     Disconnect Phone has no honest verb to send — OCBM's six MGMT verbs are
     the `MGMT_*` constants in `crates/ocbm-proto/src/lib.rs`, dispatched by `handle_mgmt` in `ccpa/ocbmd/src/main.rs`; the
     Android button used to send `MGMT_RESTART_WIRELESS` in disguise (nothing for a wired phone, a
     whole-stack bounce for a wireless one). Proposed semantics — WIRED: end the CarPlay session and
     soft-detach / re-attach the phone-facing USB gadget so iOS sees an unplug (the un-CONFIGURED
     edge `iap2d` already exits on, per CLAUDE.md's wired-tier note; `projection_up` respawns it);
     WIRELESS: drop that phone's BT ACL + Wi-Fi session WITHOUT `wireless_down` (btd's control port
     already addresses one phone, the protocol header of `crates/vendor/wireless/src/control.rs`); BOTH: a reconnect
     hold-off so the phone does not re-page at once, `MGMT_ACK` 0/1, and a `CAP_*` bit in
     `HELLO_ACK` so a host can enable the button. App hook waiting: `OcbmClient.supportsPhoneDisconnect`
     (one line: `caps and CAP_<bit> != 0`) → `OcbmClient.disconnectPhone()` →
     `CarlinkManager.disconnectPhone()` → the Control-tab button enables itself and its
     "Requires adapter firmware support" line disappears; no UI edit. Acceptance: wired session,
     press Disconnect Phone → iOS ends CarPlay within ~2 s, box emits `SEV_PHONE_ABSENT` then
     `SEV_PHONE_PRESENT`, no re-arm during the hold-off, the phone reconnects on its own afterwards;
     wireless session, same press → `BT_PHASE IDLE`, `PROJ_MODE NONE`, hostapd and hci stay up
     (no `wireless_down` in the supervisor log), the phone re-pages after the hold-off.
  2. **`MGMT_CONNECT_DEVICE [ascii MAC]` — targeted connect.** Problem: a Phones-tab card tap used
     to send `MGMT_RESTART_WIRELESS` (wrong with two bonds, a radio bounce for a wired phone); the
     cards are information-only until this exists. Evidence: btd's control port already has
     `{"cmd":"connect","address":..}` (`crates/vendor/wireless/src/control.rs`, `handle_request` `"connect"` arm)
     next to the `pair_answer` verb (its `"pair_answer"` arm) that ocbmd already relays (`send_pair_answer`,
     `ccpa/ocbmd/src/main.rs`) — the MGMT relay is the only missing piece. Semantics: forward
     to btd, `MGMT_ACK` 1 when the MAC is not bonded. App hook waiting: re-add
     `CarlinkManager.connectToDevice(mac)` → `mgmtAction(MGMT_CONNECT_DEVICE, mac)` and the card's
     `onTap`. Acceptance: two bonded phones in range, tap B → `BT_PHASE LINK_UP` →
     `CT_PHONE_IDENT.deviceID == B`.
  3. **"Phone drop on host re-SUBSCRIBE" (`PROJ_MODE NONE` then `SESSION_EVENT PHONE_ABSENT` after
     an app relaunch) — NOT reproduced, 2026-09-25.** Three timelines, all wired, the phone never
     left the bus in any: (a) kill-without-STOP relaunch (`adb install -r`): SUBSCRIBE → stale
     `WIRED_CP` replay → `NONE` +2.5 s (the box's forced re-arm teardown) → `WIRED_CP` +4.5 s →
     video +6.8 s; (b) clean CT_STOP then a 27.6 s gap (activity destroyed, relaunched): SUBSCRIBE →
     `NONE` immediately (box already idle) → `WIRED_CP` +2.5 s → video +4.3 s; (c) the app's
     clean-slate reset (CT_STOP, 6 s wait, new nonce): SUBSCRIBE → `NONE` immediately →
     `WIRED_CP` +5.2 s → video +7.1 s. So after a clean STOP plus ≥ 6 s the box is idle at
     re-SUBSCRIBE and `PROJ_MODE NONE` is the replayed CURRENT state, not a transition, and the
     host returning "before teardown completes" is only the (a) shape — which still kept the phone.
     If `PHONE_ABSENT` recurs, look at the supervisor's L1 `phone_reset` rung
     (`tools/session_supervisor.sh`, `escalate`) and the gadget un-CONFIGURED edge, not at host timing.
  4. **Is the A/V session key re-issued to a same-nonce re-SUBSCRIBE?** Still open. Today's resets
     rotate the nonce, so they cannot answer it; the same-nonce paths are the heartbeat
     re-SUBSCRIBE after `SEV_HOST_GONE` (`OcbmClient.startHeartbeat`) and a USB-blip reattach.
     Test: with carplayd alive, starve the heartbeat past `HEARTBEAT_GRACE` (10 s), let the client
     re-SUBSCRIBE, and check whether a fresh seam key arrives (`onSessionKeyed`, the lanes'
     `keys=` counter) before any A/V frame.

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

- **`av_dropped` / `lo_dropped` are stderr-only.** The app's CCPA tab polls `MGMT_GET_INFO`, so the
  two backpressure counters that matter most for diagnosing A/V loss are invisible to the only UI
  that polls the box.
  Source: `docs/carplay/02_SESSION_LIFECYCLE.md` §3. Verified open 2026-08-16:
  `grep -n "fn box_info_json" -A45 ccpa/ocbmd/src/main.rs` → neither counter is in the emitted JSON.

- **`wireless_down` may orphan the bring-up's forked children — PARTLY STALE (2026-10-28).** The
  teardown branches `pkill` only `btd`, `carplayd` and the `btmon` bench lever (`rx-connect` was merged
  into `carplayd` 2026-09-08). The 2026-08-16 wording named `wlan_on.sh`, `bt_on.sh` and
  `attach_bluetooth.sh` as survivors, but `wireless_up` no longer calls the first two: bring-up runs
  `radio_hal.sh wifi_ap_on` and `radio_hal.sh bt_on` in a detached wrapper (`tools/session_supervisor.sh`).
  Whether children forked by `radio_hal.sh` (the vendor attach helper) can survive a teardown was not
  checked: UNVERIFIED. The obvious `pkill` fix is the IW416 wedge the file warns about elsewhere, so a lock is
  probably the right shape.
  Source: `docs/ops/05_AUDITS.md` Finding B. Verified open 2026-08-16:
  `grep -n "pkill" tools/session_supervisor.sh`; re-read 2026-10-28.

- **The sub-16-byte opcode-0 decrypt lane never advances its counter — PARTLY CLOSED (2026-10-28).**
  The plaintext passthrough is gone: since `949112b` (2026-09-03) a `<16` byte VideoFrame body is
  logged and skipped (`continue`) in the `else` arm of `if body_size >= 16` in the `0 =>` opcode match
  of `crates/vendor/receiver/src/session.rs` (`screen` frame loop). What remains: that arm still does
  not advance `counter`, so if the phone did count such a frame the box and host nonce counters would desync.
  `docs/ops/05_AUDITS.md` §5 records the `>= 16` gate (with its nonce-advance placement) as a byte-faithful
  port of Apple's `AirPlayReceiverSessionScreen.c`, so this may be correct as is; unresolved.
  Source: `docs/ops/05_AUDITS.md` §"Where the agents corrected the audit" #3. Verified open 2026-08-16;
  re-read 2026-10-28.

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

- **The `carplayd → :9001 → ocbmd` forward path is still local TCP loopback.** The copy elimination
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

- **`altDisplayPanels[]` is parsed but never emitted; `showsInstruments` and `initialURL` are
  hardcoded box constants on a legacy `displays[]` entry.** This is a doctrine gap (box-decided where
  the app authors the value). **Caveat, do not lose it:** the code's own comment records docs/carplay/03_SDK_GROUND_TRUTH.md §5's
  "root cause" framing as REFUTED — cluster content already works via `showUI` query parameters — so
  anyone reviving the emission must first establish what it adds.
  Source: `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` §Roadmap item 4, `docs/carplay/03_SDK_GROUND_TRUTH.md` §5, `R-26-2`, `R-49-2`. Verified open 2026-08-16:
  `grep -rn "alt_display_panels\|displayPanels" crates/vendor/receiver/src/{vehicle_config,info}.rs`
  → parsed with the comment "PARSED since 2026-08-10; still not emitted"; `info.rs` has no
  `displayPanels` key and a test named `alt_display_panels_are_parse_only_today`.

- **Second main view area is APP-DRIVEN (2026-09-05, later the same day); the bench lever is the
  app-less fallback.** `vehicle_config.rs::apply` now carries `mainVideoStream.viewAreas[1].viewArea`
  (+ our `initial` extension key) into `DeviceConfig::main_view_area_2`, `info.rs::view_area_2`
  prefers it over `CARPLAY_VIEWAREA2` / `/tmp/carplay_viewarea2` (which since 2026-10-19 is read ONLY
  when no host config is in force, `DeviceConfig::host_configured`, D3) and refuses an uncontained rect with
  the same `***` log the lever gets (a refused config area declares ONE area — it does not fall
  through to stale lever state), and `view_areas_enabled()` auto-arms `viewAreas` for a declarable
  second area the way a real inset does. Host side: `VehicleConfigModel.viewArea2{Enabled,X,Y,W,H}`,
  validated by `ViewArea2Rule` in severity order (containment → all four values EVEN → positive dims,
  each a teardown → the PRODUCT floor 800x480 landscape / 480x800 portrait, a lockout) and emitted
  only when enabled AND legal, so the OFF document
  is byte-identical (fixture guard). `events::request_view_area` answers the Dock button with
  `updateViewArea` — device-proven with `1600x960@800,0` on 2400x960 and `600x400@240,760` /
  `1080x1600@0,160` on 1080x1920 (`docs/carplay/06_AV_PIPELINE.md` §3, "Bench lever, 2026-09-05").
  Still open: the Settings VIEW for the five keys (model half landed, view half in flight),
  `initial` authoring from the app (box parses it; the model does not emit it), `updateDisplayPanels`
  live re-negotiation, and the type-112 second cluster.
  Source: `docs/carplay/06_AV_PIPELINE.md` §2–§4, `docs/carplay/03_SDK_GROUND_TRUTH.md` §10, `docs/carplay/02_SESSION_LIFECYCLE.md` §"Apple's session/lifecycle model".

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

- **App-owned CarPlay pairing peer store.** Today `carplayd`'s `DiskPeers`
  (`ccpa/carplayd/src/main.rs` ~:342-439) persists `id → Ed25519 LTPK` on box flash, and
  `PeerSaver::save_peer` (`crates/vendor/pairing/src/setup.rs` ~:29-30) is infallible because
  pairing is already cryptographically complete when it runs. Owner decision (2026-09-02): the
  host app owns the store, per app install (macOS `UserDefaults`-class storage; Android app
  storage), cleared on app uninstall; the box keeps peers in memory only — no box-side disk
  fallback, so an app-less bench boot re-pairs; the app supports removing a peer, and the box
  refuses pair-verify for any peer not in the last pushed list. Mechanism: a `peers:` list in the
  `CT_SUBSCRIBE` YAML the app already re-pushes on every SUBSCRIBE (`crates/ocbm-proto/src/lib.rs`
  `CT_SUBSCRIBE`), staged by `ocbmd` next to the existing config file and read by carplayd's
  `load_device_config`; a new box→host `SEV_PEER_ADDED{id, ltpk}` on `CT_SESSION_EVENT` when
  pair-setup completes; `DiskPeers` → `MemPeers` (trait shape unchanged). Touch list: `ocbm-proto`,
  `ocbmd`, `carplayd`, `host/MacHost` (VehicleConfig/Settings push + event handling),
  `host/CarlinkAndroid`. Status: planned, not started.

---

### iAP2 & metadata

> *(2026-10-28: this block pointed at a non-existent "Phase 4" for two `iap2-core/src/message.rs`
> defects, the params-6/7 "closed dead end as a class" self-contradiction and the hardcoded Identify
> serial. Not re-evaluated; UNVERIFIED. `message.rs` now derives the accessory name suffix from the Wi-Fi MAC,
> falling back to the box serial, so the serial half may be stale.)*

- **Identify param 30 declares only subs 0 and 1, so `lane_guidance` is inert and road/destination
  names cannot populate.** Apple's sub 8 `MaxLaneGuidanceStorageCapacity` reads "Must be included to
  receive Lane Guidance instructions", and subs 2/3 gate `CurrentRoadName`/`DestinationName`.
  **⚠️ "`lane_guidance` is inert" is CONTRADICTED BY THE CODE DESIGN and by a claimed capture whose
  evidence is not archived — see [False claims](#false-claims-found-while-verifying).**
  *(corrected 2026-10-19: what holds today — param 30 still declares only subs 0/1
  (`message.rs:800-808`) and is still ungated, so road/destination names still cannot populate. What
  does NOT hold — `lane_guidance` is a `Trigger::RidesOn("route_guidance")` feature at tier
  `extended` (`features.rs:496-499`) and is declared in param 7 as `0x5204`; the archived
  `2026-07-25_SUCCESS_metadata_declaration_accepted.txt:22` shows `lane_guidance` in the accepted
  declaration. Whether iOS then SENDS `0x5204` is UNVERIFIED (no archived evidence): no file under
  `docs/ops/captures/` contains `0x5204`/LaneGuidance data, only the 07-25 declaration line.)*
  *(2026-10-19 live: 0x5204 DID arrive at tier extended on the tunnel arm without param 30 sub 8 — box counted 9 and 16 in two bursts, app Lanes card populated; still no archived raw capture, UNVERIFIED (no archived evidence) as a file. Road/destination names remain unpopulated.)*
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

- **RouteGuidance selectors — selector 3 (ExitInfo) VERIFIED LIVE 2026-10-19; selectors 4/5 untested.**
  *(Supersedes the 2026-10-19 morning entry "subscribes selectors 0/1/2 only, so ExitInfo ... cannot
  arrive", written from code; history: that entry said the subscribe was the gate — now proven for id 3.)*
  Live (tier extended, AirPlay-tunnel arm via carplayd, highway route): with selectors 1,2 only (30 B
  tunnel frame) a 13-maneuver 0x5202 burst carried ids 0..11, no 12/13; after adding selector 3
  (`metadata.rs:350-365`, 0x5200 body 14 -> 18 B, frame 30 -> 34 B) iOS delivered ExitInfo — the app's
  "Exit Info (raw)" card showed `exitInfo: Exit 9` on maneuver idx 6 (type 53) of an 18-maneuver
  route in two bursts; box logged 36x 0x5202, 16x 0x5204. Gate real, selector 3 suffices, parser
  (0x5202 id 13: `metadata.rs:732`, `tlv.rs:309`) works end to end. Selectors 4/5: 25 wire 0x5201 frames
  had ids 0..20 only while carkitd's dict held 21 and 23 => they gate ids 21+ (not yet sent). Full
  record: `docs/carplay/03_SDK_GROUND_TRUTH.md` "Latent correctness bugs" item 4 (RouteGuidance selector gap). Code state: selector 3
  sent unconditionally (interim placement, committed in `bbf3325`; corrected 2026-10-19 — this said "uncommitted"). Logs/screenshots hold route data and are not
  archived: UNVERIFIED (no archived evidence) beyond the box-parser JSON. Parsers still drop 0x5201 ids
  15/16/17/18/20/22/23/25/26 and 0x5202 ids 10/11/12; `tlv.rs` is narrower (0x5201 ids 1-12 and 19,
  0x5202 ids 1-7 and 13). Follow-ups are the six items below.
  - **OPEN (i) make selectors 3/4/5 app-driven (doctrine).** Selector set is hardcoded; per the
    app-pushed-config doctrine (`docs/carplay/04_CAPABILITIES_AND_CONFIG.md`) it should be pushed by the app.
  - **OPEN (ii) archive a redacted raw capture of 0x5202 id 13 and 0x5204** into `docs/ops/captures/`
    (arm `/tmp/carplay_rg_hex` or `CARPLAY_RG_HEX`; lever at `metadata.rs:367-423`; strip route/road/destination names).
    *(2026-10-19 later: raw id-13 wire bytes were also seen on a second accessory type, a stock CPC200 with patched ARMiPhoneIAP2 v6, ledger R-49-13; that raw hex is likewise NOT archived, so this item stays OPEN.)*
  - **OPEN (iii) test selectors 4 and 5** by sending them; check whether ids 21 (timezone) and 23 (charging list) appear.
  - **OPEN (iv) test an off-ramp route** (maneuver type 8/22/23); only one highway exit (type 53) seen, and
    NEXT MANEUVER card rendering of exit text was not observed. *(The stock-CPC200 v6 test, R-49-13, also saw
    only one numbered highway exit; off-ramp types remain untested there too.)*
  - **OPEN (v) 0x5202 id 12 absence / lane-to-maneuver linking.** Pre-change 13-maneuver burst had no id 12
    though 5 lane records (LaneGuidanceIndex 0..4) arrived; no wire link. The app's "for maneuver #N"
    (`MetadataWindow.swift:1082`) is its own reading of the 0x5204 index. Does id 12 appear with other selectors/routes?
  - **OPEN (vi) iPhone-side capture drops burst frames.** The accessoryd/carkitd capture missed the
    0x5202/0x5204 burst (stream drop), so id 13 was never seen as raw wire bytes. Find the drop cause or capture another way.
    *(Partly superseded 2026-10-19: for the OCBM box this stays open; on a stock CPC200 the same iPhone-side capture DID catch
    the 0x5202 frames and id 13 was seen as raw wire bytes, R-49-13 — so the drop is not universal; cause on the OCBM test still unknown.)*

- **C-3 — RESOLVED 2026-09-01: `iap2d` now passes the app-pushed vehicle identity on the wire.**
  `ccpa/iap2d/src/main.rs` calls `message::build_ident_info_with` (`Action::SendIdentify`) and
  `build_ident_info_excluding_with` (`Action::RetryIdentify`) with the app-pushed
  `vehicle_identity()` (docs/carplay/04_CAPABILITIES_AND_CONFIG.md C-3). `iap2d` still passes
  `message::accessory_name("CarLink")` at both Identify sites — that half of the original claim
  stands; only the tunnel and BT drivers still pass the baseline identity.
  Source: `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` §3 row C, the former "§4" of this file (no longer exists). Verified open 2026-09-01:
  `grep -rn "build_ident_info_with" crates ccpa` → hits both `ccpa/iap2d/src/main.rs` call sites
  plus a `#[test]` in `config.rs`; `grep -n "accessory_name" ccpa/iap2d/src/main.rs` → two
  hardcoded call sites, unchanged.

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
  `grep -rn "cellularSupported\|faceTimeAudioEnabled\|initiateCallAvailable" host/MacHost/` →
  zero hits; the same grep over `--include="*.rs"` hits `iap2-core/src/metadata.rs`.

- **`0x4E0D WirelessCarPlayUpdate` and `0x5700`/`0x5701` are declared but not acted on.** *(Narrowed
  2026-10-28: `0x5702`/`0x5703` ARE acted on — `crates/vendor/wireless/src/bt_driver.rs` answers
  `RequestAccessoryWiFiConfigurationInformation` (0x5702) with `AccessoryWiFiConfigurationInformation`
  (0x5703) carrying the live AP config, and `av::ensure_av_layer` follows it; that is the Wi-Fi handoff
  itself. Only the accessory-initiated pair 0x5700/0x5701, scaffolded in `wifi_handoff.rs`, is unused.)*
  Source: `docs/carplay/02_SESSION_LIFECYCLE.md` §10. Verified open 2026-08-16:
  `grep -rn "0x4E0D" --include="*.rs" crates ccpa` → every hit is a spec-table entry or a declaration
  list; no handler.
  *(corrected 2026-10-19: "no handler" is too strong for 0x4E0D — `crates/vendor/iap2-core/src/metadata.rs`
  (the `0x4E0D =>` arm of the device-update parser) parses it into a metadata record that is forwarded
  to the host pane. What remains true: nothing on the box ACTS on it (no state change, no behaviour).)*

---

### Wireless transport — the RCS DataStream and the BT→WiFi path

- **An inbound RCS `'sync'` obliges a `'rply'`; the box logs and does not send one — BLOCKED, not
  merely unstarted.** `_controlServer_sendResponseInternal` writes `messageType = 0` while the
  phone's only RCS inbound dispatch accepts `'cmnd'`/`'died'`; both cannot be true, and a malformed
  accessory frame has already wedged this link once, so guessing wrong is asymmetrically expensive.
  Source: `docs/carplay/05_METADATA_AND_CONTROLS.md` §5 item 1, the former "§5 (#15b)" of this file (no longer exists). Verified open
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

- **`/tmp/carplay_transport` is claimed at `ensure_av_layer` entry rather than in the
  `carplayd_up` success branch — ACCEPTED DESIGN (owner decision 2026-10-28), not a defect.** The flag is
  written before the spawn is confirmed, with a rollback on failure instead. The early write is
  deliberate: it closes a race where the wired supervisor's `arm()` guard saw no flag and could replace a
  correctly configured wireless `carplayd` mid-handoff (comment `#1.6` in `ensure_av_layer`). Moving it
  to the success branch would reintroduce that race. Kept listed so the audit finding is not re-raised.
  Source: `docs/ops/05_AUDITS.md` §"2026-08-01" Finding B. Verified open 2026-08-16:
  `grep -n "carplay_transport" crates/vendor/wireless/src/av.rs` → written at the top of
  `ensure_av_layer`, rolled back only in the `else` of `if carplayd_up`.

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
  `grep -n "hostAuthorableFeatures" -A4 host/MacHost/carlink_macOS/OCBM/AirPlaySetupSession.swift`
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
  hardcodes `wlan0`.** `btd`, `bt_on.sh` and `ocbmd`'s `bt_name_from()` each derive a
  name independently, and because the supervisor execs `btd` after `radio_hal.sh bt_on`
  the controller advertises `CarLink-<suffix>` rather than the seam's `ccpa-<4hex>`. The suffixes can
  diverge too, not just the prefixes.
  Source: `docs/wireless/01_BT_AND_RADIO.md` §6c/§7, `R-57-1`. Verified open 2026-08-16:
  `grep -rn "carplay_ident" ccpa crates tools pi host` → four hits, all in
  `radio_hal.sh`/`radio_ap_up.sh`, none in Rust; `grep -n "wlan0" crates/vendor/wireless/src/av.rs crates/vendor/wireless/src/box_identity.rs crates/vendor/iap2-core/src/message.rs ccpa/ocbmd/src/main.rs`
  → `const WLAN_IFACE: &str = "wlan0"` plus three `/sys/class/net/wlan0/address` reads.

- **There is no reversible radio verification GATE in the baseline, run while still on NCM — ACCEPTED
  DESIGN (owner decision 2026-10-28).** The baseline installs the seam and *reports* the resolved
  platform, but nothing gates the install on a pass/fail radio result or reverts on failure. This is
  deliberate: `tools/ncm_base_install.sh` says the report is "Informational only - nothing gates on it",
  because an unrecognised variant must still install (it keeps its own vendor dispatcher) and a failing
  report line must never skip the safety checks that follow it. Kept listed so it is not re-raised.
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

- **AA stream self-restart (landed 2026-09-04, `AppDelegate.scheduleAARestart`).** ocbmd forwards only
  changes of the owner flag on a 500 ms poll, and a wireless-AA re-bootstrap flips it
  `wireless-aa → '' → wireless-aa` within 2 ms, so a session that ended on its own got no new mode
  edge and the pump dropped the phone every 30 s ("no host app opened the AA stream"). The app now
  reopens the stream itself (2/4/8/10 s back-off) while the last reported mode is still an AA mode.
  Exercised on device the same day: after the phone refused a tier-4 H.264 declaration and closed
  the transport, the app logged "reopening the stream in 2 s" and the phone re-attached on the retry.
- **HFP uplink clock drift (open, minor).** During a call the box pads about one 20 ms silence
  packet per 5 s on the SCO uplink (`uplink underrun: 1 packets ... in the last 5 s`): the app's
  48 kHz→8 kHz capture delivers ~49.8 frames/s against the controller's 50. Inaudible so far; if
  it ever matters, resample against the SCO clock rather than the capture clock.
- **AA telephony-over-Wi-Fi experiment — CLOSED 2026-09-04**, see `docs/androidauto/03_WIRELESS.md`
  §6: declaring a type-4 sink makes the phone drop the transport right after service discovery.
  `AA_TELEPHONY_SINK` must never ship on.
- **Video P-chain holes (V6 renderer-off-main-thread fix): LANDED, measured on the 2026-09-04 03:21Z session: 1 eviction and 1 keyframe request at session start, 0 decode failures (down from 30 evictions with V5 alone).** V5
  (bounded IDR-protecting `AVCCFastPath.FrameFIFO`, depth 3, both decode/enqueue hand-offs) fixed the
  original depth-1 latest-wins drop and was measured on device: drops collapsed to a start-of-session
  burst of 30 `evict-oldest-P`, all on the ENQUEUE hand-off, in the first 25 s, then none — pointing at
  the main thread as the consumer stalling on window/appearance/format-ready work during that window.
  V6 moves the renderer hand-off (`drainEnqueue`) off main onto a dedicated `renderQueue`, so nothing
  on the frame path touches main any more; latency fields were renamed honestly in the same pass
  (`wrapLatencyMs`/`handoffLatencyMs`, `wraplat=` in `AVmon`, replacing the V5 `declat`/`decodeLatencyMs`
  names, which measured a buffer wrap, not decode). **Next relaunch must confirm:** the start-of-session
  enqueue-queue burst is gone, `dropFps`≈0 in steady state, and `handoffLatencyMs` stays inside one
  frame interval. Detail: [`../host/00_MACOS_HOST_APP.md`](../host/00_MACOS_HOST_APP.md) "Host decode
  pipeline".

> *(2026-10-28: this block pointed at a non-existent "Phase 4" for three host-app defects. Of them,
> `host/ocbm-host` `do_hello` now sends `host_instance_nonce()` rather than a constant, so that one looks
> CLOSED; `sendHello()` in `OCBMClient.swift` and `FieldInfo.inertKeys` were not re-evaluated, UNVERIFIED.)*

- **`AppDelegate.carPlayView(_:didMultiTouchTwo:)` is an empty stub — pinch and two-finger scroll are
  captured and dropped.** `CarPlayView` emits the delegate from nine sites; `AppDelegate` implements
  it with `{}`. **Its stated cause in docs/host/00_MACOS_HOST_APP.md is WRONG:** the box ships Apple's two-finger descriptor
  with a unit test against Apple's fill order, so this is host-side work, not box work.
  Source: `docs/carplay/06_AV_PIPELINE.md` §5 Phase 2, `docs/host/00_MACOS_HOST_APP.md` Tier 1 #5, `R-15-1`, `R-17-1`. Verified open 2026-08-16:
  `grep -rn "didMultiTouchTwo" host/MacHost/carlink_macOS/App/*.swift` → nine emit sites in
  `CarPlayView.swift`, one implementation in `AppDelegate.swift` with an empty body.

- **Touch aspect is still derived from the advertised/persisted resolution, not from the decoded
  frame.** `CarPlayView.videoAspect` — which defines `videoRect` and therefore every normalized touch
  coordinate — is seeded and updated only from `VehicleConfigModel.persistedMainResolution()`.
  Source: `docs/host/00_MACOS_HOST_APP.md` Tier 2 #8. Verified open 2026-08-16:
  `grep -rn "updateVideoAspect" host/MacHost/carlink_macOS --include="*.swift"` → one caller,
  `MainWindowController.applyResolution`, itself driven by `persistedMainResolution()`.

- **The bounded resubscribe with an atomic `OCBMAVDecrypt.reset()` never landed.** `OCBMAVDecrypt` has
  no `reset()` at all, so a resubscribe cannot re-lockstep the ChaCha counter; the retry rides the
  1 Hz heartbeat and is bounded only by session lifetime.
  Source: `docs/carplay/02_SESSION_LIFECYCLE.md` §"Supporting layers" (P1 residue), `R-11-1`. Verified open 2026-08-16:
  `grep -n "func reset" host/MacHost/carlink_macOS/OCBM/OCBMAVDecrypt.swift` → zero hits. *(2026-10-28:
  that grep now hits one `static func reset()`, which only clears the cached screen geometry for
  `get viewarea`; it does not reset the ChaCha counter, so the item stands.)*

- **The session log still carries no build stamp — only `Version: 1.0`.** docs/ops/05_AUDITS.md's own conclusion
  called this "the cheapest fix here" after three sessions were lost to a stale binary; it never
  landed.
  Source: `docs/ops/05_AUDITS.md` §1. Verified open 2026-08-16:
  `grep -rn "CFBundleVersion\|buildStamp\|buildDate" host/MacHost/carlink_macOS/App/` → nothing;
  `FileLogger.preamble()` prints `CFBundleShortVersionString` only.

- **`CT_RADIO` (0x16) is complete box-side but has no app-side caller.** Until the Settings-toggle
  wiring lands, the app's only radio controls are quit, `wireless: false` at next connect, and the
  automatic off-on-app-loss.
  Source: `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` §3. Verified open 2026-08-16:
  `grep -rn "sendRadio" host/MacHost host/CarlinkAndroid tools` → exactly one hit, the definition
  in `OCBMClient.swift`.

- **`FileLogger` has no in-session size or rotation cap.** Only an age-based sweep exists (delete
  files older than 14 days at startup), so one long session can grow a file unbounded.
  Source: `docs/ops/05_AUDITS.md` §"FIXES APPLIED — DEFERRED". Verified open 2026-08-16:
  `grep -n "prune\|fileSize\|truncate\|maxFile" host/MacHost/carlink_macOS/App/FileLogger.swift`
  → only `pruneOldLogs()`.

- **Media-port coordination — the app still cannot choose the media ports.** The box pre-binds and its
  own local response doubles as the oracle; the host copies `dataPort`/`controlPort` verbatim and
  passes non-authorable types straight through. This is the one genuinely-open clause left in this file's
  "Open design items".
  Source: `docs/ops/04_OPEN_ITEMS.md` §"Open design items", `docs/carplay/01_OCBM_PROTOCOL.md` §"RTSP channel". Verified open 2026-08-16:
  `grep -n "Pre-bind + oracle" -A4 crates/vendor/receiver/src/relay.rs`;
  `grep -n "copyPort" host/MacHost/carlink_macOS/OCBM/AirPlaySetupSession.swift`.

- **No NACK retransmit.** Recovery is keyframe-request-on-seq-gap only; there is no bidirectional NACK
  channel on either end.
  Source: `docs/host/00_MACOS_HOST_APP.md` Tier 4. Verified open 2026-08-16:
  `grep -rni "nack\|retransmit" --include="*.rs" --include="*.swift" .` → every hit is iAP2
  link-layer; zero in any A/V or OCBM path.

- **Decoder color attachments (709 / 601-4 / sRGB) are not set.** Color is left to VideoToolbox
  inference; the only attachment touched is the per-sample NotSync flag.
  Source: `docs/host/00_MACOS_HOST_APP.md` Tier 4 ("cheap insurance"). Verified open 2026-08-16:
  `grep -rn "ColorPrimaries\|TransferFunction\|YCbCrMatrix" host/MacHost/carlink_macOS --include="*.swift"`
  → zero hits.

- **No rate-matched ring buffer for long-session A/V clock drift; the behaviour on drift is to DROP.**
  The audit said "add one if it appears"; the audio player's own comment names clock drift as the
  reason it caps and drops.
  Source: `docs/host/00_MACOS_HOST_APP.md` Tier 4 (conditional). Verified open 2026-08-16:
  `grep -rni "drift\|ringBuffer\|rateMatch" host/MacHost/carlink_macOS --include="*.swift"` → one
  hit, `AudioPlayer.swift`'s drop comment.

- **The OCBM frame header's `seq` (offset 12, u32) is written by every endpoint and read by none.**
  Distinct from the `SEAM_MAGIC` per-video-frame `u64` seq, which *is* read. Still the dead-field
  footgun the audit flagged.
  Source: `docs/host/00_MACOS_HOST_APP.md` Tier 4 OCBM cleanup. Verified open 2026-08-16:
  `grep -rn "\.seq" --include="*.rs" crates/ocbm-proto ccpa/ocbmd host/ocbm-host | grep -v "self.seq\|seq: "`
  → one hit, a unit assertion.

- **The SOM/EOM single-frame contract is neither asserted on the host nor implemented as coalescing.**
  `OCBMReassembler` surfaces `flags` on `OCBMFrame` and does not validate SOM/EOM, so a fragmented
  frame would be delivered silently truncated. *(Corrected 2026-10-28: "no consumer reads the field" was
  wrong — `OCBMClient.deliver` reads `frame.flags & OCBM.fNewSource` for the video, alt-video, audio and
  voice feeds. Only the SOM/EOM bits are unread.)*
  Source: `docs/host/00_MACOS_HOST_APP.md` Tier 4 OCBM cleanup. Verified open 2026-08-16:
  `grep -rn "\.flags" host/MacHost/carlink_macOS --include="*.swift"`; re-read 2026-10-28.

---

### Tests, tooling & install

> *(2026-10-28: this block pointed at a non-existent "Phase 4" items 3 and 10. Item 3,
> `ocbm_install.sh` not installing `iap_role_switch`, is CLOSED (see the box-daemons note above). Item 10, a
> documentation coverage gap, is undefined and dropped.)*

- **The Swift config emitter has no unit-level coverage — PARTLY MITIGATED (2026-10-28).**
  `tests/run_tests.sh` now runs `tools/check_app_yaml_fixture.py` first, which fails the suite when the
  Rust-side literal copy of the app's emitted document is stale (regenerated by
  `tools/regen_app_yaml_fixture.py`); that guards the emitted document, not the emitter's code paths.
  `tests/run_tests.sh` still compiles `VehicleConfig.swift` but not `SettingsWindow.swift`, where the emitter actually lives — and
  app/box schema drift is the repeated failure mode on this workstream. docs/carplay/04_CAPABILITIES_AND_CONFIG.md calls this the
  highest-value open item on workstream C. A built, mutation-verified proof of concept is preserved
  in `scratchpad/`, but its golden string is stale.
  Source: `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` §3 row C. Verified open 2026-08-16:
  `grep -n "VehicleConfig.swift\|SettingsWindow" host/MacHost/tests/run_tests.sh` → one hit;
  `grep -n "iapConfigYAML" host/MacHost/carlink_macOS/App/SettingsWindow.swift` → the emitter is
  in the uncompiled file.

- **`EMITTED_KEYS` is an allowlist, not a contract — 36 keys pinned against 77 the host reads.**
  *(corrected 2026-10-19: re-ran the grep below, it now yields 77, not 76; `EMITTED_KEYS` still 36 —
  `awk '/const EMITTED_KEYS/,/^\];/' crates/vendor/iap2-core/src/metadata.rs | grep -c '^\s*("'`.)*
  Renaming any of the ~41 unpinned keys blanks a pane with the suite green.
  Source: `docs/ops/05_AUDITS.md` §6. Verified open 2026-08-16: the `const EMITTED_KEYS` table in
  `crates/vendor/iap2-core/src/metadata.rs` has **36** `("kind", "key")` rows, against
  `grep -oE '(str|int|dbl|num|bool)\("[a-zA-Z]+"\)' host/MacHost/carlink_macOS/App/MetadataWindow.swift | grep -oE '"[a-zA-Z]+"' | sort -u | wc -l`
  → **76** on 2026-08-16, **77** on 2026-10-19.

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

- **The cross-language ChaCha20 known-answer test is half written — PARTLY CLOSED (2026-10-28).**
  The Swift half exists: the section "OCBMAVDecrypt — ChaCha20-Poly1305 known-answer (external CryptoKit
  vectors)" in `host/MacHost/tests/main.swift` opens a vector sealed by CryptoKit, not by `OCBMAVDecrypt`. There is
  no Rust-side vector (the same hex appears in no Rust test), so the Swift decrypt is not pinned against
  the Rust encrypt that the box actually runs.
  Source: `docs/ops/05_AUDITS.md` §"Desk-side prep". Verified open 2026-08-16:
  `grep -rn -i "known_answer\|chacha" host/MacHost/tests/main.swift` → no matches; re-read 2026-10-28.

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
  on a healthy box.** Reviving them means setting it in the `carplayd` spawn env first.
  Source: `docs/ops/02_TESTING.md` §"The discriminator", `R-46-1`. Verified open 2026-08-16:
  `grep -rn "CARPLAY_EVENTS_LOG" --include="*.rs" --include="*.sh" . | grep -v docs/` → four hits,
  all readers; `grep -n "carplayd" tools/session_supervisor.sh` → the spawn env is
  `OCBM_FWD_ENC=1 $CM $LT $MB`.

- **`tools/ocbm_boot.sh` is 104 lines behind `ccpa/rootfs/script/ocbm_boot.sh` (138 vs 34 on 2026-10-28; was 110 vs 34).** The rootfs copy
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

- **The stock-derived CCPA rootfs network surface is unhardened — weak default PSK, unauthenticated
  `telnetd`, bare dropbear, no AP isolation, no iptables.** (Scope, 2026-10-28: this is the production
  rootfs. The separate `baseline/` image differs: telnetd is rescue-only and bound to the NCM address, and
  dropbear takes a key or a documented dev password; see `baseline/README.md`.) `radio_ap_up.sh` refuses to BLANK the passphrase but only
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
- **The old "Phase 1" (device identifiers and the vendor firmware backups) is closed in full** by
  owner decision. Nothing from it belongs in this index. (2026-10-28: the phase list itself is not in
  this file; this bullet is its only trace.)
- **The `MGMT_ENTER_NCM` second-reboot (2026-09-03) is CLOSED BY OWNER DECISION — not to be
  investigated.** A second, unexplained reboot ~60 s into the resulting NCM boot happened once in
  three cycles that hour and `/tmp` did not survive to record a cause; kept for the record only.
- **Numeric-comparison SSP: box-initiated interactive answer is NOT the default, by owner decision
  after measurement (2026-09-04).** iOS refuses to hold a box-initiated numeric-comparison request
  open for a human to answer on the accessory (`pairingComplete result:162` instantly). The box
  confirms its own side immediately by default; the interactive wait lives on only behind
  `BT_SSP_INTERACTIVE=1` / `pairing: numeric_comparison_interactive` as a bench lever. Full
  measurement: [`../wireless/01_BT_AND_RADIO.md`](../wireless/01_BT_AND_RADIO.md).

---

### Found closed — documents still call these open

Every entry here was listed as open, planned or deferred by a source document (or by a status banner)
and is shipped in the current tree. Listing them is the point: a stale "open" is exactly the false
claim this index exists to remove.

**Moved here from "Open items" by the 2026-10-28 audit (code and git evidence):**

- **`connect_seam` is numeric-only.** `connect_seam` in `ccpa/ocbmd/src/main.rs` parses a `SocketAddr` and its
  doc comment reads "Literal addresses ONLY". Introduced in `949112b` (2026-09-03).
  Verified: `git log -S'Literal addresses ONLY' -- ccpa/ocbmd/src/main.rs`.
- **W9 and the "hardcoded Raspberry Pi BT MAC" are closed in code.** `ACCESSORY_BT_MAC` no longer exists;
  `accessory_bt_mac()` in `crates/vendor/wireless/src/bt_driver.rs` reads this unit's own
  `/sys/class/bluetooth/<hci>/address` once and falls back to a locally-administered placeholder, so a
  foreign address never goes on the wire (`949112b`). No archived hardware run of the byte-pinned BT
  Identify with the new value: hardware validation UNVERIFIED.
  Verified: `grep -n "fn accessory_bt_mac" crates/vendor/wireless/src/bt_driver.rs`.
- **`nightMode` / `rightHandDrive` — CLOSED.** `nightMode` was dropped from the emitted YAML by owner
  decision (2026-09-02); no key exists on the wire for the box to drop. `rightHandDrive` is a CarPlay `/info`
  boolean (`kAirPlayKey_RightHandDrive`, `AirPlayCommon.h:1103` in R14G17); `info.rs` emits it, the app derives
  it from `driverPosition == right`, and it was DEVICE-VERIFIED 2026-09-05 (2400x960 wireless, iOS moved the
  app rail, status bar and app-grid button to the right edge, owner-confirmed). The earlier tail of this
  item saying "nothing is device-proven" and that `FeatureMatrix` must change was stale: `FeatureMatrix.swift`'s
  `(.driverPosition, .carPlay)` arm is `.supported`/`.proven("2026-09-05", ...)`. Full account:
  `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` §rightHandDrive.
  Verified: `grep -n "right_hand_drive" crates/vendor/receiver/src/info.rs`.
- **The macOS app pushes `hidConfig.touchScreenSupportsMultiTouch`** (default `false`, emitted by
  `iapConfigYAML` in `host/MacHost/carlink_macOS/App/SettingsWindow.swift`). The other half of the gap, the
  empty `AppDelegate.carPlayView(_:didMultiTouchTwo:)` stub, is still open (see above).
  Verified: `grep -n "touchScreenSupportsMultiTouch" host/MacHost/carlink_macOS/App/SettingsWindow.swift`.
- **The USB read loop no longer clears the pipe on a timeout.** `USBTransport.swift` takes the
  `kUSBTransactionTimeout` arm with `continue` and an explicit "NO ClearPipeStallBothEnds" comment; the error
  branch clears, then sleeps 20 ms before retrying. Residual: the error branch clears on any non-timeout
  read error, not only `kIOReturnPipeStall`.
  Verified: `grep -n "ClearPipeStallBothEnds\|usleep" host/MacHost/carlink_macOS/USB/USBTransport.swift`.
- **Metadata seam deltas keep their order.** `MetadataWindow.swift` hops to the main actor with a
  FIFO-preserving `assumeIsolated` (comment: an unstructured `Task { @MainActor }` "has no cross-task
  ordering guarantee"). Verified: `grep -n "assumeIsolated\|cross-task ordering" host/MacHost/carlink_macOS/App/MetadataWindow.swift`.
- **`OCBMAudioStreamFormat.bits` is read** (`OCBMAVBridge.swift`, `Int(fmt.bits) / 8`).
  Verified: `grep -rn "fmt.bits" host/MacHost/carlink_macOS/OCBM/`.
- **`ocbm_install.sh` installs `iap_role_switch`** (the manifest lists `accessory_init/iap_role_switch.armv7` ->
  `/usr/bin/iap_role_switch`). Verified: `grep -n "iap_role_switch" tools/ocbm_install.sh`.

**From this file's own tracking (previously listed above under "Box daemons"):**

- **Pairing-hold latch is DEPLOYED, not staged.** `rfcomm_uspace::pairing_aware_connect` (now
  `crates/bt-common/src/rfcomm_uspace.rs`) is on the box and measured against iOS 27
  (2026-09-03/04): [`../wireless/01_BT_AND_RADIO.md`](../wireless/01_BT_AND_RADIO.md) §"Connect hold".

**From `docs/ops/05_AUDITS.md` §DEFERRED and §"DEAD CODE inventory":**

- **MicCapture re-wired, not retired.** `AppDelegate` instantiates it and pipes `onPCMData` to
  `client.sendMicPCM`, started and stopped from `onUplinkGate`.
  Verified 2026-08-16: `grep -rn "MicCapture\|onPCMData" host/MacHost/carlink_macOS`.
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
  `grep -rn "SendOutcome\|onSubscriptionState" host/MacHost`.
- **C1 timeout shrink + C4 blind-retry removal landed**, tagged in-source by audit id.
  Verified: `grep -n "C1\|C4" host/MacHost/carlink_macOS/USB/USBTransport.swift`.
- **V1 AVCC fast path + V4 backpressure landed** as `Video/AVCCFastPath.swift`.
  Verified: `grep -rn "V1\|V4" host/MacHost/carlink_macOS/Video/`.
- **W1 RFCOMM reassembly landed** — a persistent accumulator with a bounded desync drop.
  Verified: `grep -n "reassembl" crates/vendor/wireless/src/bt_driver.rs`.
- **The W-set landed** — ordered thread joins before A/V teardown, and the AV latch vouches for
  rx-connect by tracked pid. *(Corrected 2026-10-28: `AV_RX_CONNECT_PID` was removed with the `rx-connect`
  merge into `carplayd` on 2026-09-08; the latch now vouches for `carplayd` through `AV_AIRPLAYD_PID`.)*
  Verified: `grep -n "AV_AIRPLAYD_PID" crates/vendor/wireless/src/av.rs`.
- **The TSan scheme is enabled.** Verified: `grep -rn "enableThreadSanitizer" host/MacHost/` → two
  `YES` entries in the shared xcscheme.
- **Finding B's direction 3 landed** (reap `rx-connect` by both name forms) — docs/ops/05_AUDITS.md's own inline
  note says "only fix direction 3 is unlanded", which is off by one; direction 2 is the open one.
  (2026-10-28: moot — `rx-connect` no longer exists, so there is nothing left to reap by name.)
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
  Verified: `grep -rn "24000" --include="*.rs" crates/vendor/receiver ccpa/carplayd`;
  `grep -rn "startCapture(" host/MacHost/carlink_macOS --include="*.swift"`.
- **Backpressure-not-drop is in the tree**, not "BUILT … staged" — per-stream out-queues, video gated
  on its own backlog, audio never gated. Verified: `grep -n -i "backpressure\|never gated" ccpa/ocbmd/src/main.rs`.
- **The knob/telephony descriptors, `lane_guidance` and param 30 all shipped**, so `R-06-1`'s
  remaining-work list is itself partly stale. Verified:
  `grep -n "fn knob_descriptor\|fn telephony_descriptor" crates/vendor/receiver/src/info.rs`;
  `grep -rn "lane_guidance" crates/vendor/iap2-core/src/features.rs`;
  `grep -n "ident_info_wireless_transport_declares_route_guidance_param30" crates/vendor/iap2-core/src/message.rs`.
- **AltVideo shipped in full** — `CH_ALT_VIDEO 0x24`, seam `:9005`, dedicated host decoder and window
  — although `docs/host/00_MACOS_HOST_APP.md` Tier 4 still bundles it into one open bullet with VDC and NMEA GPS.
  Verified: `grep -rn "CH_ALT_VIDEO" crates/ ccpa/ host/MacHost`.
- **This file's other two "Open design items" are genuinely resolved** — the `receiver_core` rootfs
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
  Verified: `grep -n "helloAcked\|HELLO_ACK" host/MacHost/carlink_macOS/OCBM/OCBMClient.swift`.
- **The wireless research's "Still open" items 1 and 3 are closed** — the Bonjour responder on `wlan0`
  ships in `carplayd` (was `rx-connect`; that crate no longer exists), and the link-key store reuses `carplay_peers.bin`. Verified:
  `grep -n "_airplay._tcp" ccpa/carplayd/src/discovery.rs`;
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
  send `0x5204`" without param 30 sub 8. A recorded (UNVERIFIED — no archived evidence) capture claim says otherwise.**
  `docs/ops/05_AUDITS.md` §9 records the 2026-07-29 session as delivering `0x5204 LaneGuidanceInformation` ×12 alongside `0x5201` ×574, as the
  first live confirmation that a subscribe-less `Trigger::RidesOn` feature receives data. The param-30
  expansion is still genuinely open; the *reason* stated for it is empirically wrong.
  **corrected 2026-10-19: UNVERIFIED (no archived evidence).** The 2026-07-29 capture that this rests
  on is not in `docs/ops/captures/` (`rg -c '5204|LaneGuidance' docs/ops/captures` → only the 07-25
  declaration line; the wired 07-24 log carries `0x5201`/`0x5202` RX but no `0x5204`). The ×12/×574
  counts are therefore a recorded claim, not a checkable fact. What the tree does support is that
  `lane_guidance` is declared and rides `route_guidance` (`features.rs:496-499`); `message.rs:750`
  already carries a "CORRECTED" note and no longer asserts "can never send" as fact.
  **Superseded 2026-10-19 (live):** the "x12" figure stays UNVERIFIED (no archived evidence), but 0x5204 delivery
  without param 30 sub 8 was live-observed that day (9 and 16 messages in two bursts; Lanes card populated) — no archived raw capture.
  Verified 2026-08-16: `grep -n "0x5204" ../ops/05_AUDITS.md ../carplay/03_SDK_GROUND_TRUTH.md`;
  `grep -n "iOS can never send" crates/vendor/iap2-core/src/message.rs`.

- **docs/ops/05_AUDITS.md's Phase-2 banner over-closes.** It reads "⚠️ EVERYTHING IN THIS SECTION SHIPPED — do not
  treat any item below as outstanding work", but `R-50-2`'s evidence covers only the desk-side batch
  and the `Protocol/` deletion. At least six items in that same section have not shipped: I2 link
  dedupe, W9 BT MAC, `CT_INPUT_NACK`, `connect_seam` numeric-only, ocbmd `opt-level=2`, and the
  cross-language ChaCha20 KAT — listed above as of 2026-08-16; `connect_seam` and W9 have since closed
  and the KAT is half written (2026-10-28). Separately, docs/ops/05_AUDITS.md's Finding-B inline note
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
  description anywhere in the tree. Tried: `grep -rn "U9\|U13" host/MacHost/ ../ops/05_AUDITS.md`
  → one hit, docs/ops/05_AUDITS.md's own line. Contrast V1/V4 and C1/C4, which ARE tagged in-source.
- **Hardware-observation gaps that no grep can settle.** The ACL-drop outcome (plan_A test 3), the
  remaining app/phone checklist steps (plan_A §5 tests 2–8 and 10), the app-pushed metadata tier
  ("landed 2026-08-10, NOT yet hardware-validated") *(2026-10-19: landing confirmed — `f3fa61d` 2026-08-10 adds `iap2-core/src/config.rs` with the `metadata.tier` push and `features.rs` arming; hardware validation still UNVERIFIED (no archived evidence))*, and `docs/carplay/06_AV_PIPELINE.md`'s ○-marked audio formats
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

*(Scope, 2026-10-28: unless a line says otherwise, these are CPC200-CCPA results — NXP i.MX6UL, ARMv7.
The C2Air (Allwinner V821, riscv32) and the Pi / AAOS port are tracked in `c2air/` and `pi/docs/`. OCBM has
not been run on the C2Air's new baseline image.)*

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
  overlay iteration (on a CCPA: NXP i.MX, ARMv7; there is no FEL and no U-Boot console, and no
  recovery except a kernel-initramfs recovery boot or an SPI programmer, so a brick without one is
  permanent — see `01_RECOVERY.md`); plus OCBM `push` for binaries once `ocbmd` is running. **Persistent FHS install**
  (daemons in `/usr/sbin`, tools in `/usr/bin`, scripts in `/script`) + turnkey boot: a cold boot comes
  up with a live OCBM link, zero bootstrapping (reboot-proven). **CORRECTED 2026-08-16 — the installer:**
  the current path is `tools/ncm_base_install.sh` then `tools/ocbm_install.sh --full` (reversible trial
  before finalize; `--full` also places `btd` and the `radio_detect.sh`/`radio_hal.sh`/
  `radio_ap_up.sh` seam). `tools/install_fhs.sh` still works but is the older OCBM-era subset —
  `ocbmd`/`iap2d`/`carplayd` + `iap_role_switch` only (it once also placed `rx-connect`, merged into
  `carplayd` 2026-09-08), no wireless stack and no radio seam.
  See [`../ops/01_RECOVERY.md`](../ops/01_RECOVERY.md).
- **MFi** — a genuine Apple MFi coprocessor drives authentication; the project is MFi-version agnostic.
  On a CCPA it is the 2.0C part at `/dev/i2c-1 @0x11` (DeviceVersion 0x03, 945 B certificate, 128 B
  signature), driven from userspace by five users (`iap2d`, `btd`, the receiver's AirPlay-tunnel handshake,
  `carplayd`'s `LocalMfiSigner`, and `ocbmd`'s `CH_MFI` relay), all serialised on `/tmp/carplay_mfi.lock`.
  With `CARPLAY_MFI_ADDR` set, three of them instead reach a CCPA acting as an MFi oracle over USB-NCM
  (`ccpa/mfid`); a Raspberry Pi 4 AAOS host and a Linux workstation reached full CarPlay A/V that way. The
  C2Air's chip reads DeviceVersion 0x07 (608 B certificate, 64 B ECDSA signature).
- **Phone-side iAP2 handshake — reaches Identified** against a real iPhone (SYN-ACK → cert/0xAA01 →
  sign/0xAA03 → 0xAA05 → 0x1D01 → 0x1D02), and holds the link. Unblocked by the `link.rs::parse`
  coalesced-read fix.
- **Adapter pairing + key derivation** — `ccpa/carplayd` on `ncm0` completes pair-setup/pair-verify +
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
3. ~~**Adapter pairing path.**~~ **DONE.** `ccpa/carplayd` reuses receiver_core's `ControlServer`
   (pair-setup/verify + `/info`) + `pairing`/`rtsp`/`mfi`, backed by a `LocalMfiSigner` on the local i2c
   chip. Full pair-setup → pair-verify → MFi-SAP → ChaCha20 session key ran end-to-end against a real
   iPhone; mDNS `_airplay._tcp` on `ncm0` via `rx-connect` (since 2026-09-08 a thread inside `carplayd`). **Disk-backed PeerStore**
   (`/etc/carplay_peers.bin`) persists pairing — known device reconnects pair-verify-only.
4. ~~**Session key + encrypted A/V over OCBM.**~~ **DONE.** The box forwards the encrypted media
   untouched + hands the per-stream key over `CH_VIDEO`/`CH_MEDIA_AUDIO`; a Rust debug receiver
   (`ocbm-host avdec`) decrypted host-side (video + audio, 0 failures).
9. ~~**Session lifecycle & host-presence management.**~~ **DONE.** (**NOTE 2026-08-16** — this item and
   "Metadata" below are both numbered `9.` in the original list; the duplicate is left as written. Where
   the banner above says "item 9" it means **Metadata**.) Host-app-driven: SUBSCRIBE (CH_CTRL)
   → IDLE→projection (`projection_up.sh`) → ARM (`session_supervisor.sh` starts carplayd);
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
   uplink exists too: host `MicCapture.swift` → OCBM → carplayd's `MIC_INGEST_ADDR 127.0.0.1:9112`.
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
   `vehicle_config.rs` and applied per control connection by `carplayd::load_device_config` — runtime
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
   Current pick-up: the "Open items" section of this file. This
   realizes the future-proofing target of README §7 (SDK-evolving SETUP logic app-side).
9. **Metadata** (NowPlaying / Nav) — ✅ DONE (surfaced via `features.rs`-generated params 6/7 + subscribes; see docs/carplay/05_METADATA_AND_CONTROLS.md). ~~not yet surfaced; likely needs a subscribe/command or a channel `AvSession` doesn't currently expose.~~
10. **Wireless CarPlay** (BT-RFCOMM iAP2 + WiFi SoftAP + credential handover). ✅ DONE for transport,
    pairing and A/V (see docs/carplay/05_METADATA_AND_CONTROLS.md). ~~**⚠️ The METADATA plane regressed 2026-07-31 → 2026-08-10** (a
    scid guard rejected the type-130 DataStream SETUP; tunnel stuck at `Init`, zero NowPlaying). Fix
    landed 2026-08-10, **not yet hardware-validated** — do not re-mark this line fully DONE until a
    soak shows `0x5001` records arriving.~~ **RESOLVED — the validation gate this line set was met on
    2026-08-11**: Identify accepted, `decryptFail=0`, NowPlaying and route/lane guidance arriving
    (README §Status, 2026-08-11). *(corrected 2026-10-19: the fix is commit `9a5e38e` (2026-08-10,
    "stop refusing iOS's type-130 DataStream SETUP"), now `crates/vendor/receiver/src/session.rs:902`
    `if scid == 0 && matches!(ty, 100..=102 | 110 | 111)` — exists, `git cat-file -t` → commit. The
    "validated 2026-08-11" half is UNVERIFIED (no archived evidence): `README.md` Status carries no
    2026-08-11 date or `decryptFail` figure (only the undated "metadata (NowPlaying, route and lane
    guidance, call state)" line), and no capture after 2026-08-10 is archived. The guard's introducing
    commit `5ce9d1c` resolves in no ref — it was squashed into `f88989d`.)* Regression evidence retained:
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
  shipped set is ≈**3.8 MiB unpacked** at that date (`carplayd` ~1.71 MiB, `rx-connect` ~574 KiB — since merged into `carplayd` — `iap2d` ~551 KiB,
  `btd` ~537 KiB, `ocbmd` ~443 KiB), roughly half that UPX-3.96-packed — comfortably inside
  the ~6 MB free, and run-from-`/tmp` was never needed. Numbers and method:
  [`../ops/00_BUILD_AND_DEPLOY.md`](../ops/00_BUILD_AND_DEPLOY.md).
- ~~**SETUP relay latency** — driving SETUP app-side relays the control connection box↔app over USB;
  measure it doesn't destabilize the timing-sensitive pair/SETUP/RECORD phase.~~ **RESOLVED 2026-08-08
  (docs/carplay/02_SESSION_LIFECYCLE.md): PASS** — p99 2.36 ms under real A/V load (upper-bound path), ~21× inside the 50 ms gate,
  zero timeouts/cap-clears. Go/no-go in docs/carplay/02_SESSION_LIFECYCLE.md.

### Host-app / integration (not adapter firmware)

- **Box presence reads a wired Android as ABSENT.** `phone_on_bus` (`tools/session_supervisor.sh`)
  matches only the iPhone USB id and the `android0` gadget, so a wired Android phone (a USB device
  toward the box) makes the box emit `SEV_PHONE_ABSENT` ~1.3 s into a wired AA session. The app now
  ignores that while AA owns the view (fixed 2026-09-04), but the box should not emit it at all —
  `phone_on_bus` should return present when a wired Android or an AA owner holds the session. Until
  then any other consumer of box presence is wrong during wired AA.
- **Planned (not defects) — see [`08_FUTURE_TASKS.md`](08_FUTURE_TASKS.md):** projection-aware,
  vehicle-centric Settings redesign (T1, raised 2026-09-04 — today the Configuration tab is a
  CarPlay-shaped list and every Android Auto setting is an `AA_*` environment lever); Android Auto
  metadata services (T2); wideband HFP (T3).
- **GM AAOS USB permission prompt — CORRECTED 2026-10-08: the squat below is superseded.** The owner found the squat never removed the prompt (2026-09-10) and gm_ccpa reverted to `zeno.gmccpa` (2026-09-08). Current route: an adb-written persistent grant (bench-verified 2026-10-07), see `docs/host/01_ANDROID_AND_AAOS.md`. The paragraph below is the 2026-08-17 record, kept as history, and its "Landed in the gm_ccpa receiver" sentence is no longer true. Original: **"SOLVED and DEVICE-TESTED (2026-08-17)."** The reappearing permission
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

### Android host app (`host/CarlinkAndroid`) — 2026-09-18 session

The box is STATIC this session — nothing below is a box or protocol change, only Android client
capability. `:app:assembleDebug :app:testDebugUnitTest :app:detekt :app:ktlintCheck` PASS and
`python3 tools/proto_check.py` OK gate this batch; see `docs/ops/06_CORRECTIONS_LEDGER.md` for the
per-claim corrections this session produced and retracted.

CLOSED or materially advanced (test-green; **NOT hardware-verified** unless stated):
- **`SEAM_PKT_PLAIN` (0x03) and mSBC decode landed for the Android Auto telephony lane.**
  `Msbc.kt` / `MsbcFramer.kt` (ported from macOS `MSBCCodec.swift`/`MSBCFramer.swift`) decode the
  marker, and mSBC uplink encode honours the `CT_UPLINK` codec byte. Scope correction (see ledger):
  `mic_seam_serve` in `crates/vendor/wireless/src/sco_audio.rs` gates `:9112`/`:9003`→`CH_ALT_AUDIO` to
  `ProjectionOwner::WirelessAa`/`WiredAa` only — this lane never carries CarPlay call audio, so this
  work does **not** serve the CarPlay path this repo is otherwise scoped to; it is Android-Auto-only
  client capability. Test-green only — no on-vehicle HFP/mSBC call has exercised it yet.
- `CT_PROJ_MODE` and `CT_BOX_HEALTH` no longer fall into the client's unknown-type log branch —
  both are parsed and surfaced now.
- `CMD_VIEW_AREA = 0x11` is now defined in the Kotlin client, closing one of two `proto_check`
  Kotlin gaps (verified: `python3 tools/proto_check.py` — CarlinkAndroid's only remaining gap is a
  local `F_BOTH = 0x03`, absent from `ocbm-proto`, shared with `gm_ccpa`). **The mid-session
  `CMD_VIEW_AREA` PUSH is still NOT wired** — do not read the constant landing as the feature landing.
- Cutout/safe-area geometry now reaches the wire, computed relative to the display CONTENT area
  rather than the app window (`app/.../ocbm/VehicleConfigYaml.kt`) — see the content-area correction
  in the ledger for why the window measurement was wrong.
- The voice lane no longer hardcodes AAC-ELD: it branches on the declared codec and refuses an
  undecodable stream before any sink or audio focus exists.
- The UI is resolution/orientation-agnostic via `WindowSizeClass` breakpoints, in place of the prior
  single-2400x960-landscape-panel assumption — verified on 9 panel shapes and one real 800x1280/120
  portrait AVD. **The `PanelRule`/`ViewAreaRule` portrait acceptance is a defect this exposed, not a
  feature** — see ledger.

STILL OPEN (re-checked, not newly found this session — carried forward): the host-side `mgmtLock`
gap; `CH_MGMT` typed surface (`MgmtInfo`) and `CH_FILE` `FILE_PULL` for config readback; `META_CMD`
only partly consumed; the FGS microphone-type issue; ~~wireless pairing — the Android client receives
`CT_PAIRING_CODE` but never answers with `CT_PAIR_CONFIRM`~~ **CLOSED app-side 2026-09-25:**
`OcbmClient.sendPairConfirm` + a Pair / Cancel prompt on the main screen (bytes pinned by
`OcbmClientTest`; not yet exercised against a real wireless pairing).

NEW open items:
- **`:app` cannot use `CarProjectionManager`/`ProjectionStatus`, `CAR_NAVIGATION_MANAGER` cluster
  turn-by-turn, or `ClusterHomeManager`** — all `signature|privileged`, verified on a running AAOS 15
  image, and the app is required to stay third-party. `CH_ALT_VIDEO` on a cluster display is
  therefore out of reach for this client, not a missing feature to schedule.
- **`dpi`/`diagonalInches` are detected by the Android client but never emitted.**
  `crates/vendor/receiver/src/vehicle_config.rs` has no slot for either, `/info` hardcodes
  `widthPhysical: 0` (comments in `host/CarlinkAndroid/app/src/main/kotlin/com/carlink/ocbm/VehicleConfigYaml.kt` and
  `host/CarlinkAndroid/projection/src/main/kotlin/com/carlink/projection/VehicleConfigGenerator.kt`), and the macOS
  host's own field help says `diagonalInches` is not sent to either phone
  (`host/MacHost/carlink_macOS/App/SettingsWindow.swift`, panel physical description). Open question, not a defect.
- `DisplayMode` `STATUS_BAR_HIDDEN` / `NAV_BAR_HIDDEN` are wired but not exercised on hardware —
  only `SYSTEM_UI_VISIBLE` and `FULLSCREEN_IMMERSIVE` were.
- No available panel has a cutout, waterfall edge or rounded corner, so those safe-area arms are
  JVM-test-proven only, never device-proven.
- **`host/CarlinkAndroid` ships launcher icon assets byte-identical to `carlink_native`'s** —
  confirmed by md5 (`ic_launcher_foreground.png`, `ic_launcher_background.png` match across
  `host/CarlinkAndroid/app/src/main/res/mipmap-xxxhdpi/` and the sibling repo's same path) despite
  distinct `applicationId`s (`zeno.carlink.ocbm` vs. the `carlink_native` flavor id) — the two
  installable builds are visually indistinguishable in the launcher and on the CarPlay home screen.

### Contingencies (future — if/when measurements demand it)

- **Adapter-driven SETUP — ALREADY THE SHIPPED FALLBACK, not a future contingency** (corrected
  2026-08-10; it also cited the wrong step — app-driven SETUP is step **8**, step 6 is nav voice/mic).
  When `appsetup() && seam_up()` is false the box drives SETUP locally from the app-pushed config
  (the `else` arm of the `levers::appsetup() && receiver::relay::seam_up()` gate in
  `ccpa/carplayd/src/main.rs` — grep the condition, not a line number; that file moves often);
  pushing `appDrivenSetup: false` forces it (corrected 2026-10-19: this said `CARPLAY_APP_SETUP=0`
  forces it; that env is no-config-only since D2 and no longer beats a pushed config). Historical note: if app-driven SETUP (step 8) had added too much
  latency and risk session stability, fall back to having the **adapter drive the SETUP negotiation
  locally from the on-box YAML** (the config pushed down). The fallback stays bounded to rendering the
  app-pushed config — adapter-owned SETUP capability does not grow (docs/carplay/04_CAPABILITIES_AND_CONFIG.md directive 4). Keeps the negotiation next to the iPhone (no relay latency) at the cost of
  some future-proofing (the adapter would then carry SETUP logic that evolves with SDK versions). Only
  consider if the SETUP-relay-latency measurement above shows a real problem. See `README.md` §7.
- **Decode-on-adapter fallback** (deep contingency): the box could decode/re-emit typed A/V (the
  original CCPA model) — retained only as a last resort; the i.MX6UL has no VPU, so video is forwarded
  either way and this buys little.

## HANDOFF 2026-09-05 → 09-07 — CarPlay view-area resize: the rules, the matrix, and what is left

**STATE: the resize matrix is COMPLETE (9 ACCEPTED / 1 LOCKOUT, both orientations, floor to 4K) and
the whole loop is unattended** — the app arms the rect through its own model fields, commands the
transition over the control socket, and judges the result from the decoded frame. Three things are
open and none of them block using resize: the LOCKOUT detector is wrong (Known-open 7), the
per-panel floor is unbisected (8), and the animation duration is not yet a variable (6).

Device-proven on a **CPC200-CCPA, iPhone18,4 / iOS 27.0 (24A5430a)**. The 09-05 work was **wireless**
CarPlay; everything dated 09-07 is **WIRED** (the phone is plugged into the box, so there is no
phone-side log in that arm at all). Where a claim is arm-specific it says so; the rule set below
holds in both.

### The rule set (settled)

1. **ALL FOUR values must be EVEN** — `x`, `y`, `width`, `height`. An odd value is a **TEARDOWN**:
   `kFigEndpointError_InvalidParameter -16720` originating in **`carEndpoint_copyScreenInfo:7001`**,
   propagating `setupScreenStreams:8536` -> `setupStreams:8957` -> `activateInternal:9987` ->
   `Activate_block_invoke_2:10117`. Isolated to one pixel: `356x400@240,760` renders,
   `357x400@240,760` tears down; `601x400@240,760` (odd w, far above any floor) tears down;
   `600x400@240,761` (odd origin Y only) tears down. Cause is almost certainly HEVC 4:2:0 chroma
   subsampling. **This retroactively explains every `-16720` in the project's history** — the old
   `1416x842@492,59` and `1600x842@800,59` failures both carry an odd origin Y and nothing else was
   ever wrong with them. Prior explanations (size floor, "must touch a vertical edge",
   `focusTransfer`, `cornerMasks`) are all REFUTED; see docs/carplay/06_AV_PIPELINE.md.
   **SCOPE, narrowed 2026-09-07 (wired, 4K panel): parity binds what we DECLARE, not what iOS puts
   back on the wire.** iOS's own transition animation streams odd rects freely — 275 of 292 geometry
   records in one round trip carried an odd origin or extent (`3826x2152@7,4`, `3834x2158@3,1`) with
   no teardown — and a SETTLED rect can be odd too (see "What the wired arm added" below). Do not
   "fix" a harness or a validator to reject those; the gate is on the declaration.
2. **Containment** — `x+w <= panelW && y+h <= panelH`. Violation is a TEARDOWN.
3. **PRODUCT FLOOR (owner, non-negotiable): 800x480 landscape / 480x800 portrait**, CarPlay AND
   Android Auto, orientation chosen by the area's own aspect. This OVERRIDES what iOS tolerates.
4. **No origin constraint** beyond parity + containment. No evenness-of-position rule beyond (1), no
   edge-contact requirement (a floating portrait area touching no edge renders). **Re-proven
   2026-09-07 on a 3840x2160 panel over WIRED CarPlay, which is where the free origin was previously
   only assumed**: `1280x720@200,180` (off-centre upper-left), `800x480@3040,1680` (flush into the
   bottom-right corner, `x+w`/`y+h` exactly equal to the panel) and `2560x1440@640,360` each
   transitioned and settled at exactly the armed rect, no refusals, no teardowns. Diagonal offsets
   are not a special case — the rule really is: even, contained, at or above the floor, anywhere.
5. `cornerMasks` and `viewAreaSupportsFocusTransfer` are both **orthogonal** to acceptance.

**iOS's own tolerance, for protocol understanding only — NOT the shipping rule.** Measured floors on
a 1080x1920 panel: width **350**, height **304** (`350x400` renders, `348x400` locks out;
`600x304` renders, `600x302` locks out). The extracted `385 * 0.65 * scale` / `240 * 0.65 * scale`
formula is REFUTED as the gate — it mispredicted `480x400`, `400x400` and `384x400`, all of which
render. Do not resurrect it.

**Two failure classes, never conflate:** an odd/oversized/non-positive value is a **TEARDOWN**
(session dies). Below the floor is a **LOCKOUT** — `[DBLockOut] lockOutMode updating to
viewAreaTooSmall`, string `LOCKOUT_VIEW_AREA_TOO_SMALL_MESSAGE` -> "CarPlay does not support this
display resolution." Black area, session SURVIVES.

### What the WIRED arm added (2026-09-07)

Same box, same iPhone, but the phone is plugged into the BOX — so there is no phone-side log at all
(docs/ops/02_TESTING.md). Every verdict below came from the app's control socket, the app log and the
pixels. The matrix now runs unattended end to end, with no human in the actuation path:

- **The resize button is the pill at the TOP of the CarPlay Dock rail** — two arrows pointing inward,
  above the clock, not among the app icons. Pressing it from the socket works:
  `tap 133 179` (the app's 0..10000 space, area 0 on a 1200x675 window) -> the phone sends
  `requestViewArea index=1` -> the box answers `updateViewArea index=1` -> the picture animates into
  the sub-rect. **The button moves WITH the area**, so the return press is at different coordinates
  (`tap 4109 4039` for `800x480@1520,840` in a 3840x2160 panel). A harness cannot use one constant.
- **A settled rect is not always the declared rect, and it can be ODD.** `1920x1080@1900,1060` settled
  at `1921x1081@1899,1059` and STAYED there — polled every 2 s for 24 s, geometry-record counter
  frozen at 154, so the animation had finished. The far edges landed exactly (`1899+1921 = 1900+1920
  = 3820`); iOS converged one pixel out toward the origin. Three other rects the same session landed
  byte-exact, so this is a per-rect rounding artifact of the transition's final interpolation step,
  not a correctable offset. **Compare settled rects with a ±2 px tolerance** (`rect_close` in
  `tools/va_wired_sweep.sh`); exact string equality scores a good case NO-MOVE.
- **iOS self-transitions at session start.** Unprompted, it shrank into area 1, held ~2.4 s, and grew
  back to area 0 (answered `updateViewArea -> index=0`). A harness that samples one geometry record
  instead of the settled rect reads this as a pass.
- **THE LOCKOUT FLOOR SCALES WITH THE PANEL — the product floor is not panel-independent.**
  `480x800` inside a **2160x3840** panel LOCKS OUT: iOS renders
  `LOCKOUT_VIEW_AREA_TOO_SMALL_MESSAGE` ("CarPlay does not support this display resolution") and the
  session survives, exactly as the two-failure-classes rule predicts. But the wireless measurements
  above found areas as small as `350x400` RENDERING on a **1080x1920** panel — the same panel at half
  the linear size. So the same rect can render on one panel and lock out on a bigger one, and
  `480x800` — our own declared portrait product floor — is NOT safe on a 4K portrait panel. On
  2160x3840 the floor lies between `480x800` (lockout) and `720x1280` (renders, mean luma 76.6);
  it has not been bisected. This does NOT resurrect the refuted `385 * 0.65 * scale` formula — that
  mispredicted specific rects and stays refuted — but it does mean any floor quoted without its
  panel is meaningless. Bisect before treating 800x480/480x800 as universally shippable.
- **A LOCKOUT is not a black area — do not detect it by luminance.** The banner card is sized to
  FILL the view area, so the armed rect reads mean luma 40.0 / darkFrac 0.01 (content!), while the
  panel *around* it is 95% dark. `tools/va_wired_sweep.sh`'s first thresholds (`BLACK_MEAN=12`,
  `BLACK_DARK=0.85`) therefore scored a textbook lockout as ACCEPTED, and only an eye check caught
  it. The usable discriminator is CHROMA, not brightness: a real CarPlay UI is full of coloured app
  icons, the lockout card is flat achromatic grey with white text.
- **`animationDurationMillis` is honoured literally.** Against the hardcoded 3000 we send, measured
  **2.978 s** (shrink) and **2.988 s** (grow) at ~48 geometry records/s. It is a real duration, not a
  hint. Its LIMITS are unmeasured — see "Known-open" below.

### Tooling left behind

- `tools/va_limit_probe.sh SPEC LABEL [cornermasks] [focustransfer]` — arms a view-area spec,
  captures the PHONE's log across the attempt, classifies ACCEPTED / DEGRADED / REJECTED /
  REFUSED-LOCALLY. Requires WIRELESS CarPlay (the iPhone's USB must be free). *(Corrected
  2026-10-19: it used to push `:initial` specs to the box lever `/tmp/carplay_viewarea2`, so the
  session STARTED in the area. That lever is no-config-only since D3 and ignored while the app it
  relaunches is connected, so the probe now arms through the app door (`viewarea arm` + `save`) and
  enters the area with `viewarea request 1` once the session is up — still unattended. It refuses
  `:initial` specs, and rects the app's `ViewArea2Rule` rejects — including everything below the
  800x480 / 480x800 product floor — now score REFUSED LOCALLY: sub-floor limit probing is
  app-less-only and this probe cannot do it. Not re-run on hardware since the migration.)*
  *(Tightened 2026-10-19, review: ACCEPTED now requires the `viewarea request 1` reply to be
  `{"ok":true}`, the observed rect (`get viewarea`, within `RECT_TOL`) to reach the armed rect after
  the geometry has settled from iOS's own session-start resize, no box `host viewArea index=1
  REFUSED`, and no teardown for 10 s; otherwise INCONCLUSIVE. Early exits clean up the app it
  launched and the phone-log capture.)*
- `tools/va_limit_sweep.sh` — drives panel AND area together, floor-to-4K, both orientations
  (calls the repo `va_limit_probe.sh`, no `:initial`, since 2026-10-19).
- Two traps already fixed in these, do not reintroduce: the capture must NOT be `-pn carplayd`
  (the lockout is logged by CarPlay's UI process, not carplayd), and the app-restart dwell must be
  **12s** — the supervisor's wireless teardown SIGTERM lands ~6s late and will kill the bring-up
  that follows it.
- **2026-09-07, WIRED arm:** `tools/va_wired_sweep.sh` — same matrix and even-centring arithmetic,
  but no phone capture exists in a wired session, so it verdicts from the control socket
  (`viewarea arm` + `save`, then `viewarea request 1` or a `TAP` on the Dock button), the app log,
  and the pixels of the armed rect (`shot`, or `tools/winshot.sh` — window capture by CGWindowID,
  occlusion-proof). Verdicts: ACCEPTED / LOCKOUT (black rect, session alive) / TEARDOWN /
  REFUSED-LOCALLY / INCONCLUSIVE; PNGs under `/tmp/valog/wired/<label>/`. Procedure and the two
  bench traps it encodes (sandboxed-shell launch kills the app; never score a screen region) are in
  docs/ops/02_TESTING.md "Wired view-area resize matrix". **The Dock TAP coordinates are pinned but
  no longer needed** (`tap 133 179` in area 0; the button moves WITH the area, so it is never a
  constant — `viewarea request` replaced it). **The LOCKOUT thresholds are still WRONG — see
  Known-open 7.**
- **Three harness defects the first runs exposed, all the same shape — a case that LOOKS measured
  and is not. Do not reintroduce any of them:**
  1. **Triggering before the geometry settles.** iOS runs its own view-area round trip at session
     start, so a trigger fired in the same second the session comes up lands inside that animation
     and is swallowed; the BEFORE frame is a picture of a rect nobody asked for. `settle_geometry`
     waits for the `changes` counter to hold still.
  2. **Reading `[backfill]` as this session's evidence.** The app replays box log history on
     connect, tagged `[backfill]`. A backfilled `host viewArea index=1 REFUSED` from the PREVIOUS
     case decided `P-720x1280`'s verdict. Verdict-producing scans go through `live_log`.
  3. **Trusting a panel the app silently clamped** — the `PanelRule` bug below. The sweep now
     compares `armed.panel` against what the case asked for and fails it REFUSED-LOCALLY with
     "measures nothing" rather than measuring a rewritten panel.
- **The `PanelRule` bug this cost, fixed 2026-09-07.** The app's panel envelope was per-axis
  (`VehicleConfigModel.minHeight/maxHeight` 480–2160), so `set height 3840` was silently clamped to
  2160 on `save`, every portrait area failed containment against a 2160-tall panel, the emitter
  dropped `viewAreas[1]`, and the box declared one area — while the landscape half passed 5/5 and
  the run looked healthy. The envelope is now `PanelRule` (orientation-agnostic, floor by the
  panel's own aspect) and clamps are reported (`clampNotes` in `save`/`get viewarea`,
  `pendingClamp` in `set`) instead of being silent.

### The matrix — COMPLETE as of 2026-09-07 (wired)

Both orientations, floor to 4K, **9 ACCEPTED / 1 LOCKOUT**. Every case armed through the app door
(`viewarea arm` + `save`, no bench lever) and triggered by `viewarea request` (no tap, no human).
PNGs per case under `/tmp/valog/wired/<label>/`.

| Panel | Area | Verdict |
|---|---|---|
| 3840x2160 | 800x480@1520,840 | ACCEPTED (also OWNER-CONFIRMED by eye, wireless, 09-05) |
| 3840x2160 | 1280x720@1280,720 | ACCEPTED |
| 3840x2160 | 1920x1080@960,540 | ACCEPTED |
| 3840x2160 | 2560x1440@640,360 | ACCEPTED |
| 3840x2160 | 3840x2160@0,0 | ACCEPTED (area [1] == area [0]) |
| 2160x3840 | 480x800@840,1520 | **LOCKOUT** — below iOS's floor FOR THIS PANEL |
| 2160x3840 | 720x1280@720,1280 | ACCEPTED |
| 2160x3840 | 1080x1920@540,960 | ACCEPTED |
| 2160x3840 | 1440x2560@360,640 | ACCEPTED |
| 2160x3840 | 2160x3840@0,0 | ACCEPTED (area [1] == area [0]) |

Plus four off-centre landscape cases proving the free origin at 4K (see rule 4):
`1280x720@200,180`, `800x480@3040,1680` (flush into the corner), `2560x1440@640,360`, and
`1920x1080@1900,1060` (the ±1 settled-rect case).

**The largest panel previously driven was 2400x960**, so 4K panel config in BOTH orientations is
new. The only failure is the portrait floor case, and it is a real device result, not a harness or
config fault — see the panel-scaling finding above and Known-open 8.

The WIRELESS matrix (`tools/va_limit_sweep.sh`) is still one case deep. Nothing here contradicts it —
wired and wireless differ only in carrier — but if a wireless-specific view-area difference is ever
suspected, that is the run to complete.

### Known-open, in priority order

1. **DONE — `ViewArea2Rule` corrected and verified.** Carries the owner product floor
   (800x480 landscape / 480x800 portrait, orientation by the area's own aspect) and the parity rule,
   reported in severity order: containment, parity, positivity, floor. The refuted
   `385 * 0.65 * scale` derivation is gone. `run_tests.sh` 1300 passed (at that date; 1415 on 2026-10-28); receiver PASS; docs OK.

   **Parity is enforced in the APP ONLY, and that is deliberate — do not "fix" it box-side.**
   CLAUDE.md's design doctrine: anything configurable about CarPlay is app-driven, the box presents
   app-pushed config, and box placement is EARNED (measured app-driven failure + owner approval),
   never designed-in. A box-side parity gate was written, tested and then REVERTED on owner
   direction — the app is where the logic and the options live. There has been no measured
   app-driven failure to earn a box-side copy. The app-less `CARPLAY_VIEWAREA2` bench lever
   therefore still accepts an odd rect and will tear the session down; that is a BENCH tool used by
   an operator who now knows the rule, not a product path.

2. **DONE — transition path commandable, VERIFIED ON DEVICE 2026-09-07.** `:initial` proves iOS
   accepts STARTING in an area; it does not prove iOS animates INTO one on request. Owner-confirmed
   twice by eye — `600x400` on a 1080x1920 panel, and `800x480` inside 3840x2160. The clean fix
   landed: `viewarea request <index>` on the ControlServer → `CMD_VIEW_AREA 0x11` →
   carplayd → `events::switch_view_area`, the same policy function that answers the phone's own
   `requestViewArea` (docs/host/00_MACOS_HOST_APP.md, "Bench control surface"). Two companions for
   the WIRED arm, where there is no phone log: `viewarea arm <WxH@X,Y>` writes the rect through the
   model's own fields (replacing `defaults write` against a stopped app and the `/tmp/carplay_viewarea2`
   lever), and `shot [path]` dumps the decoded frame as PNG — the only LOCKOUT detector this side owns.
   **The rebuilt carplayd IS pushed** (2026-09-07, `tools/ocbm_push.sh … /usr/sbin/carplayd 755`,
   md5 verified against the local cross-build; ocbmd unchanged). Device-proven the same evening:
   `viewarea request 1` → `[events] host viewArea index=1 accepted` → `updateViewArea -> index=1
   duration=3000ms` → the rect settles at the armed area; `request 0` returns; `request 5` is
   REFUSED box-side (`only 2 area(s) declared`) with the session healthy. All ten matrix cases ran
   on this path.
   **This also proves the accessory can drive a transition UNILATERALLY** — nothing asked. The
   earlier finding was that the phone's `requestViewArea` is advisory and the accessory is the
   authority (events.rs); this extends it: no request need exist at all. An old carplayd logs
   `unknown INPUT_COMMAND 0x11 — dropped`, so a box that has not been updated needs
   `TRIGGER_MODE=tap` in the sweep, or the trigger silently looks accepted while nothing moves.
3. **Supervisor teardown/bring-up is not serialised** and reports success for a stack that is dead
   6s later — this is the same defect as the "Restart Wireless stack" button being a destructive
   no-op. Two fixes: serialise on child reap, and verify the bring-up instead of asserting it.
4. `viewArea2*` keys are UserDefaults-only — not in the neutral profile document, so Import/Export
   does not round-trip them. Deferred because the profile schema is shared with the Settings view
   work in another session.
5. `initial` is parsed box-side but not authored by the app model.
6. **CLOSED 2026-09-09 — `animationDurationMillis` is fully characterised; there is no iOS floor or cap.** Swept on hardware (CPC200-CCPA, wired, 1920x1080, second view area): **10 ms renders the switch as an instant flicker; 10000 ms runs the full 10 s.** Honoured literally at both extremes. One nuance worth keeping: at 10000 ms the UI reaches the target geometry at roughly 5 s and then idles showing CarPlay's transition blur until the envelope expires — the duration is the ANIMATION window, not the geometry change, so the accessory gets its settled geometry earlier than the declared duration. Untested: whether a larger area delta occupies more of the envelope instead of finishing early. The shipped range is now **1000–10000** by owner decision — a product limit (sub-second reads as abrupt in a car), NOT a protocol one; 10 ms works and is simply not offered. The original text of this item follows for the record.

   ~~**`animationDurationMillis` limits are unmeasured; its FLOOR is the open question.**~~ (Corrected
   2026-09-09 — it was "hardcoded 3000, not yet a variable"; both halves are now false.) On the bench
   that night 3000 → 1000 was DEVICE-PROVEN to visibly speed up the Dock resize, the first time anyone
   varied it on hardware, and it is now a pushed value: top-level `view_area_anim_ms` in the vehicle
   config (our extension; absent = 3000, the app emits it only when it differs — the idiom `wifi_ap`
   also used until 2026-10-19, when the macOS app began emitting `wifi_ap:` on every push),
   parsed by `receiver::vehicle_config`, armed per connection into `levers::view_area_anim_ms`, and
   consumed by `events::switch_view_area` for BOTH the inbound `requestViewArea` answer and the
   `viewarea request <index>` door. Out of range CLAMPS to 0..=10000 (logged by carplayd, never a
   rejection), so the ladder below past 10000 — 30000, 60000, `INT32_MAX`, a negative — is NOT
   reachable from the app without widening that clamp deliberately; 0, 1, 100, 250, 1000, 3000, 10000
   are. iOS never acknowledges the field, so the `[events] updateViewArea -> … duration=Nms` line
   (post-clamp value) plus a stopwatch on the geometry stream is the whole measurement. What the
   sources give: `CarPlaySDK.framework` carries the key name only;
   CarKit has an explicit clamp message for the INDEX (`Resetting to first view area … out of range`)
   but **no duration-validation string anywhere in the iOS 27 CarKit or AirPlaySender extracts**;
   R14G17 predates view areas entirely; CINEMO logs it as `duration: %ums`, unsigned, so a negative
   is outside a shipping vendor's own model. Apple's Simulator and WWDC 2019-252 both use 3 s.
   Since iOS honours the value literally (measured above), the ladder worth running is 0, 1, 100, 250,
   1000, 3000 (known good), 10000, 30000, 60000, plus `INT32_MAX` and a negative — watching for
   whether 0 snaps or is refused, whether a long duration blocks a second `requestViewArea` mid-flight,
   and whether anything tears the session down. The per-value recompile that ladder needed is gone
   (push a config with `view_area_anim_ms: N` and reconnect); a per-request `[ms]` argument on the
   `viewarea request` door would only matter for sweeping values within one session.
7. **The sweep's LOCKOUT detector is WRONG and currently scores a lockout as ACCEPTED.** Device-proven
   2026-09-07 on `480x800@840,1520` in a 2160x3840 panel: `BLACK_MEAN=12` / `BLACK_DARK=0.85` never
   fired because **the lockout card is sized to FILL the view area** — the armed rect reads mean luma
   **40.0**, darkFrac **0.01** (i.e. content), while the panel *around* it is 95% dark. The verdict
   was caught only by looking at the PNG. No luminance threshold can fix this: a real render and a
   lockout are both "not black". The discriminator is **CHROMA** — a real CarPlay UI is full of
   coloured app icons, the lockout card is flat achromatic grey with white text. `tools/winshot.swift`
   `stats` needs a saturation metric and the classifier needs to use it. Until then, **every ACCEPTED
   verdict near a floor must be eyeballed** against `$OUT/<label>/after_frame.png`.
8. **Bisect the lockout floor per panel — the shipping floor may be wrong.** `480x800` locks out on
   2160x3840 but the wireless data has `350x400` RENDERING on 1080x1920, so the floor scales with the
   panel and our declared portrait product floor is not safe on a 4K portrait panel. On 2160x3840 it
   lies between `480x800` (lockout) and `720x1280` (renders). Bisect both orientations at 4K, then
   decide whether the product floor stays a constant or becomes panel-relative. Do item 7 first — a
   bisect run on a detector that cannot see a lockout produces a confidently wrong number.
