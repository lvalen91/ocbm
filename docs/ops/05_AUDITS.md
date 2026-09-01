# Audits and remediation

> **STATUS:** CURRENT · single owner for this topic. Consolidated 2026-08-31 from pre-consolidation docs 50, 48, 25; the originals are in git history and in the 2026-08-31 backup. Correct this file in place — do not add a sibling.

Code audits and their remediation. Findings that are still open live in ops/04_OPEN_ITEMS.md; this file is the record of what was audited and what changed.

## Audit remediation

<!-- absorbed: ../ops/05_AUDITS.md -->

Supersedes nothing; extends `../ops/05_AUDITS.md` and
`../carplay/03_SDK_GROUND_TRUTH.md`. This is the record of a whole-tree
static-analysis + review pass (all Rust and Swift), the fix proposals it produced, and the
Phase 0/1 remediation that has landed. Phase 2 (hardware-gated) is planned below, not yet done.

### Method

Three fan-outs, each over disjoint file sets:

1. **Audit — 12 agents** (6 Rust, 6 Swift). Each read its subsystem in full, ran the available
   static analysis (`cargo clippy` per crate/target; `xcodebuild`; `swiftlint` is not installed),
   cross-checked the Swift↔Rust wire contracts, and verified every claim at a `file:line`.
2. **Fix proposals — 6 agents**, one per subsystem slice, each producing line-accurate diffs with a
   risk class and a verification plan.
3. **Implementation — 6 agents** for the mechanical, non-hardware batch (Phase 1), on disjoint files.

The project's standing discipline held throughout: nothing touched the byte-pinned BT-time Identify,
the `features.rs` generation contract, or `SENT_MSG_IDS`/`RCV_MSG_IDS`. The NEON-off decision
(`.cargo/config.toml`) and the order-of-authority rules (CLAUDE.md, docs/carplay/02_SESSION_LIFECYCLE.md) were respected.

### Headline results

- **Rust:** 0 correctness-class clippy findings across all 15 crates (the pre-fix backlog was 15
  style warnings, now cleared). The pairing crypto (HKDF labels/nonces/signing orders), the A/V
  forward-path nonce discipline, and the i2c NAK-retry/timing all verified correct against their
  references.
- **Swift↔Rust:** the OCBM framing (61 constants + every field layout) and the A/V ChaCha20-Poly1305
  crypto framing match **field-for-field**, and decrypt is loss-immune by construction (seq is the
  nonce). No divergence except the latent sub-16-byte opcode-0 case (see below).
- **Recently-fixed areas verified intact:** TEARDOWN empty-array semantics, EVENT-mutex poison
  tolerance (all four sites), Zero-Ack, and the docs/wireless/00_WIRELESS_CARPLAY.md/41 wireless fixes.
- 266 Rust tests + the Swift build pass after Phase 1.

### The lost-command mechanism (handoff Open item 1) — solved

The audit assembled a complete causal chain for "one `setLimitedUI` left the host, ocbmd's counter
climbed, airplayd never saw it, zero write failures logged." Each link is confirmed at a `file:line`:

1. Every OCBM send runs `writeQueue.sync` into a blocking USB write whose worst case (~10–11 s, two
   `WritePipeTO` attempts + a `ClearPipeStallBothEnds`) **exceeds the box's 10 s heartbeat grace**
   (`ocbmd main.rs` `HEARTBEAT_GRACE`). One wedged write therefore *guarantees* the box declares the
   host GONE.
2. On GONE the box sets `subscribed=false` and silently discards all CH_INPUT until a fresh SUBSCRIBE.
3. The host's GONE handler re-subscribes but **never clears its own `subscribed`**, and a *failed*
   SUBSCRIBE still set `subscribed=true` — so `sendCommand` kept writing into the void reporting
   success.
4. The blind full-frame retry in `writeBulkRaw` could duplicate a partial first write, making the box
   reassembler silently resync (drop a frame) while the host returned `true`. No IOKit sync-write
   error code guarantees zero bytes transferred, so a safe retry is impossible.
5. The UI reported "Sent to iPhone" from a *discarded* send result.

**Phase 1 closed links 3 (partial: `subscribed` truth is landed via C2 design; the timer race C5 is
done) and the observability half.** The arithmetic fix (shrink raw-write timeouts so the cumulative
gap stays under 10 s) and the retry removal are Phase 2 (need one device session to confirm the box
does not legitimately NAK > 500 ms under 4K load).

### Where the agents corrected the audit (confidence-raising)

1. **BT MAC is not cosmetic.** `ACCESSORY_BT_MAC` feeds param 17 of the byte-pinned BT Identify —
   reclassified to a hardware session with `idevicesyslog` capture and a ready revert.
2. **The four extra `limitedUIConfig` keys can never be emitted.** The box parses
   `pairedDevices`/`themeCustomization`/`automakerSettings`/`automakerSettingsInfoButton` only for YAML
   round-trip; Apple's `airPlayElements` getter excludes them. They ship (Phase 1) with an explicit
   "never appears in /info" caption, not as capability toggles.
3. **The sub-16-byte opcode-0 divergence is a box-side bug.** Host nonce tracking survives regardless
   (seq rides each message); the real latent bug is the box's legacy decrypt lane not advancing its
   counter on the plaintext-passthrough path (permanent desync if it ever fired). Host half landed
   (V6 classifies it as protocol-invalid); box half deferred to the Rust hardware session.
4. **The metadata timeouts were a red herring.** The loopback connect fails instantly when the
   consumer is absent; shortening timeouts cannot close the artwork-duplication hole. Link-layer
   duplicate suppression (which the code's own comment already named as "the real fix") is the only
   correct candidate — designed with full u8-wraparound analysis, deferred to Phase 2 (I2).

### New finding surfaced during implementation — eld-codec ELD-SBR ASC

The mic-uplink ELD encoder's AudioSpecificConfig is **`f8f0312c00bc00`**, not the documented
`f8f03000`. Cause, confirmed with a standalone C probe against fdk-aac 2.0.3: `csrc/eld_shim.c` never
sets `AACENC_SBR_MODE`, so fdk's auto-mode (−1) enables ELD-SBR at 16 kHz/mono/24 kbps. Frame length
stays 480 either way; only the ASC changes. Confirmed **pre-existing** (identical on the unmodified
file, via `git stash`) — so the shipping box binary plausibly already emits the SBR ASC, diverging
from the "f8f03000" claim in the docs. Fix is a one-line shim change (`AACENC_SBR_MODE=0`) but is a
wire behavior change that needs the phone to confirm ELD acceptance — **deferred**, not applied.

### Phase 0 + Phase 1 — landed (commits on `main`)

| Commit | Area |
|---|---|
| `5ce9d1c` | Baseline — the uncommitted 07-29/30 review + Simulator-verification work (92 files) |
| `9022eaf` | Build system — eld-codec cc-rs toolchain lookup + rerun-if-env-changed; build.sh builds all 6 box binaries |
| `1d67278` | ocbmd/ocbm-proto — hoisted 64K per-wake buffers, table CRC-32, poll(POLLOUT) pacing, bounded ssp_enabled, truncated-FILE_CLOSE reject, Pty let-else, one Instant/pass |
| `871384a` | iap2d/airplayd/iap2-core — bounded tx(), fatal i2c-open, empty-challenge Ignore, hermetic policy test, clippy; airplayd plock folded in |
| `de3a46b` | receiver/rtsp — bounded uplink writes, plock sweep, single plist parse, gated RCS hex dumps, wantsDedicatedSocket log, rtsp FrameError taxonomy, clippy |
| `34657a5` | wireless/pairing/mfi/eld — checked setsockopt, bounded flock/arbiter, strict i2c ioctl, reopen-race close, constant-time SRP M1, RNG error variant, M5 replay guard, frame_len-0 guard |
| `75dde09` | host OCBM/USB/audio — heartbeatTimer race, CT_STOP logging, stats Mutex, frame oversize guard, unhandled-frame logs, device-manager logs, Sendable hygiene, V5 decrypt-fail keyframe, V6 short-body, V9 dead code, A1/A2/A3/A5 audio |
| `3e40b90` | host UI — CCPABridge sessionEnded+stale, refresh timeout, committed-config geometry, inert "Name", off-main log export, Siri strong-capture, stderr redirect, captioned limitedUI keys, dPad tooltip, artwork-id byte compare, Adapter Info placeholder |

Verification: clean tree, all 6 box binaries build, 266 Rust tests pass (five new regression tests),
`xcodebuild` clean.

Note: the ocbmd and receiver areas were double-dispatched by the harness (duplicate agents on the same
files); they converged on identical verified state and the union was committed.

### Phase 2 — ~~planned (not done)~~ **LANDED (corrected 2026-08-16)**

> **⚠️ EVERYTHING IN THIS SECTION SHIPPED — do not treat any item below as outstanding work.**
> The desk-side batch, the `SendOutcome` enum, the `OCBMSessionCoordinator.swift` split and the
> `Protocol/`-layer deletion all landed; the text below is kept only as the record of what was
> planned. Item-by-item detail: [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-50-2`.

#### Desk-side prep (unit-testable, no hardware)
- **ocbmd OutQueue/`frame_into` refactor** (proposal B+C): convert `out_hi`/`out_lo` to the OutQueue
  cursor type and add `ocbm-proto::frame_into` to delete `scratch` and one full-frame memcpy per A/V
  frame, plus the last O(n²) drain. Wire-identical; unit-testable.
- **Swift `SendOutcome`/`onSubscriptionState` seam** (C2/C3 + the API U1–U3 consume): the completion
  API, truthful `subscribed`, and the heartbeat-tick SUBSCRIBE retry — all validated by a fake-transport
  unit test. `SendOutcome` enum is `{ sent, droppedNotSubscribed, writeFailed }` (the UI needs to
  distinguish the two failures; the transport side widens its `Bool` to this).
- **New OCBM test suite**: framing round-trip + resync, seam-parser fixtures, the fake-transport
  subscribe state machine, and a cross-language ChaCha20 known-answer test (needs one tiny Rust-side
  fixture-printing test). Requires moving `OCBMSessionCoordinator` to its own file so the CLI harness
  doesn't drag in IOKit.

#### Hardware session 1 — wired box
ocbmd OutQueue refactor + opt-level=2 measurement; R4 pre-auth gate (Apple 470); I2 link dedupe
(artwork transfer regression check); W1 RFCOMM reassembly (unit-validated, then smoke).

#### Hardware session 2 — wireless cycle
W-set (thread supervision, latch/rx-connect respawn) + R2 SESSION-lock chip-op restructure, against the
docs/wireless/00_WIRELESS_CARPLAY.md regression checklist and a byte-compare of the 0x1D01. W9 (real BT MAC) is its own pinned-Identify
session with a ready revert.

#### Hardware session 3 — host device
The lost-command chain (C1 timeout shrink + C4 retry removal, with SendOutcome plumbed to the UI); V1
AVCC fast path + V4 backpressure (forced flush-recovery + main-stall tests); U9/U13b input changes.

#### Structural (after the transport work)
Delete the dormant `Protocol/` layer (~2,097 lines of legacy 0x55AA), extracting `TouchAction`/`CommandID`/
the LE helpers to a new `InputTypes.swift`; then the OCBM test suite, a TSan scheme, and an optional
swiftlint config.

#### Also deferred
- eld-codec `AACENC_SBR_MODE=0` (the ASC finding above) — needs the phone.
- The `CT_INPUT_NACK` protocol addition (box tells the host instantly when it drops input while GONE) —
  cross-component, schedule after the chain fixes prove out.
- `connect_seam` numeric-only (E) — pending a host-app CH_IP usage check.
- ocbmd `opt-level=2` — measure-first, after the hot-path refactor lands.

### 2026-08-01 — on-hardware validation + two new wireless findings

The full Phase 1 + Phase 2-desk batch was deployed to the box (UPX 3.96, backup at
`scratchpad/box_backup_20260801/`) and validated live: clean cold boot, ocbmd CTRL/FILE/CONSOLE/ECHO
(35 MB, 0 mismatches — the OutQueue refactor proven on-wire), a full **wired** CarPlay session
(video 18,040 / audio 47,239 frames, 0 decrypt failures), and a **wireless** session (BT→WiFi handoff,
A/V flowing, 0 failures). Also fixed a papercut: `ocbm-host`'s no-arg default PID (0x1520 → 0x2d00).

Two behavioral gaps surfaced during wireless testing, both root-caused (read-only investigation):

> **⚠️ Finding A's proposed FIX is SUPERSEDED and was never landed.** Box-initiated paging
> conflicts with docs/carplay/04_CAPABILITIES_AND_CONFIG.md directive 3; the mgmt-Add-Device fix below was verified NEVER LANDED in
> code (the docs/wireless/01_BT_AND_RADIO.md Model-B loop superseded it), and radios now power on only on app command.
> The diagnosis below — accept-only bring-up, bonds persisting via `LOAD_LINK_KEYS` — still stands.
> Full reasoning: [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-50-1`.

**A. The box never BT-autoconnects a known device (accept-only).** Bonds *do* persist — link keys are
written to `/etc/carplay/bt_link_keys` (JFFS2) and reloaded via `LOAD_LINK_KEYS` on bring-up, so bond
loss is NOT the cause. The gap is initiation: bring-up ends at `hciconfig hci0 piscan` (connectable+
discoverable) and ssp_agent's mgmt setup does only POWERED/BONDABLE/CONNECTABLE/SSP — no mgmt Add
Device / accept-list / auto-connect, and RFCOMM is listen/accept-only (never `connect`). Apple's model
is head-unit-anchored (../wireless/00_WIRELESS_CARPLAY.md:292 lists an accessory-initiated "BT connect → raise AP → hand off" as a
supervisor job); that page-the-known-device step was never built. **Fix:** mgmt Add Device (0x0021,
action=0x02 auto-connect) for each stored bond right after `LOAD_LINK_KEYS` in `ssp_agent::run` — the
store already has each BD_ADDR; no persistence change needed.

**B. Wired↔wireless can't switch without a reboot.** *(**FIXED 2026-08-16 note:** this was resolved
**43 minutes** after the section was written — `9f67f0d` 08:14 → `91e9a07` 08:57 on 2026-08-01.
`phone_on_bus()` now runs `wired_iphone_on_usb && return 0` FIRST, refuting the "short-circuits before
checking the USB bus" diagnosis; `preempt_wireless_for_wired()` exists; and the `pkill -x rx-connect`
reap landed. Only fix direction 3 is unlanded, deliberately. Text kept as the record.)* The
dual-transport design assumed a session
arbiter that preempts the loser; that arbiter *server was never implemented* (`wireless/src/main.rs:10-13`),
so `carplay-wireless` runs `GrantedStandalone` and only tears down on SIGTERM (host-app-absent or the
CCPA "restart wireless" button) — never on a USB plug. Concrete stale-state:
  1. `/tmp/carplay_transport` sticks at `"wireless"`: `av.rs:236` (`ensure_av_layer`) writes it at entry
     before the spawn is confirmed; the only authoritative clear is `teardown_av_layer()` (SIGTERM-gated).
     And `phone_on_bus()` (session_supervisor.sh:92) short-circuits `true` on flag==`"wireless"` *before*
     checking the USB bus, so the supervisor is structurally blind to a USB plug during wireless →
     `arm()`/`projection_up` never run → wired can't take over while the app stays present.
  2. Leaked wired `rx-connect`: supervisor spawns it bare-name (`arm()`, `tools/session_supervisor.sh:209`
     — not :168, which is inside `kill_session`'s pkill) but the wireless side reaps by full path
     (`av.rs:148` in `teardown_av_layer`, `av.rs:349-351` in `ensure_av_layer` — not :336) → two
     `_airplay._tcp` advertisers.
  **Fix directions:** a supervisor preempt edge (genuine 05ac iPhone on USB + flag==wireless → SIGTERM
  carplay-wireless → teardown clears flag+latch+daemons → `arm()`; stop `phone_on_bus` short-circuiting
  so the raw USB check is still consulted); write the transport flag only in the `airplayd_up` success
  branch (av.rs:364), not at entry; reap rx-connect by both name forms. These are their own work items
  (touch the byte-pinned-Identify-adjacent wireless crate + supervisor — validate on a wireless session).

---

## Code review

<!-- absorbed: ../ops/05_AUDITS.md -->

Twelve independent reviewers across the box daemons, the host app, the tests and the docs. Every
finding was then put to additional reviewers before being acted on; deterministic claims (a parameter
id, a parser's output, a build timestamp) were settled by execution instead, which is stronger.

Reviewers were told explicitly that "no findings" was an acceptable result and that a finding required
an exact file, line, concrete triggering input, and the resulting wrong behaviour. Three returned
NO FINDINGS on their primary surface. Two claims were refuted outright.

---

### 1. The empty panes were a stale binary, not a defect

Telephony, Power and Device read empty during the 2026-07-25 session. Three sessions of hypotheses —
JSON key mismatch, parser bugs, a mid-session reset — were all wrong.

Xcode's `LogStoreManifest.plist` records exactly two Release builds of the host app:
`Jul 25 17:57:16` and `Jul 29 16:40:24`. The session log began at **17:52:42**, before either. A
string probe of the binaries that existed at that moment confirms `batteryChargeLevel`,
`wirelessCarPlayAvailable` and `battery.100.bolt` (the Power category's SF Symbol) were all absent.

The `power` and `device` records reached the app, hit `default:`, and were filed under **Other**.

docs/carplay/04_CAPABILITIES_AND_CONFIG.md already warned that `host/CarPlayHost/build/` is stale and that `strings` is unreliable for
confirming a change landed. Both traps fired. The app's session log prints only `Version: 1.0`; a
build stamp would have made this a one-minute diagnosis and is the cheapest fix here.

**Live gap, separate from the above:** `communications` (0x4158) emits 17 keys and the host reads 8.
The nine unread are the capability half — `cellularSupported`, `faceTimeAudioEnabled`,
`initiateCallAvailable`, `endAndAcceptAvailable`, `holdAndAcceptAvailable`, `swapAvailable`,
`mergeAvailable`, `holdAvailable`, `faceTimeVideoEnabled`. If the one observed 0x4158 carried only
those, the pane maps nothing. The raw-record fallback now renders the payload, so this needs no
hardware time to settle.

---

### 2. Repeated groups are encoded flat — `list_update` and `app_discovery` are wrong

`countExpressionEnum` in Apple's spec archive has four values. The legend in `tools/i2mspec_dump.py`
was hand-guessed and **wrong**. The real mapping:

| value | meaning |
|---|---|
| 0 | zero or more — repeated |
| 1 | exactly one, required |
| 2 | one or more — repeated |
| 3 | zero or one, optional |

Both 0 and 2 repeat; they differ only in minimum. Three independent lines agree:

1. **Apple's own notes.** `LaneAngle` (count 0): *"Specifies angles (or a single angle)"* against its
   sibling `LaneAngleHighlight` (count 3): *"the angle to highlight"*, singular.
2. **Our own device-accepted Identify.** Params 16 and 17 are count-0 groups, and `message.rs` builds
   each as one TLV whose value holds the fields directly, no index wrapper. iOS accepts it.
3. **CINEMO CT5.** `CinemoIAPCommunicationsListUpdate.java` declares
   `CinemoIAP2RecentsListItem[] RecentsList` — a flat array. `CinemoIAPAppDiscoveryUpdate.java` the
   same. `CinemoIAPLaneInformation.java` has `short[] laneAngle` for the count-0 param and
   `getLaneAngleHighlightValid/Value` for the count-3 one.

So a repeated group is emitted once per entry at the parent level, its value holding the entry fields
directly. `list_update` and `app_discovery` walk one level deeper than that and emit **zero** records —
confirmed by execution against spec-conformant bodies. `lane_guidance` in the same file parses the
identical shape correctly, so the file contradicts itself.

`app_discovery_emits_one_record_per_app` bakes the wrong wire shape into its fixture, which is why it
passes. It must be rewritten with the code or it will lock the bug back in.

Also confirmed by execution: `device_update` parses 0x4E0E's two `<utf8>` identifiers with `uint()`
under wrong key names, and `media_library` applies 0x4C04's field layout to 0x4C01, whose single
top-level param is a group.

**Settled by disassembly.** CINEMO's `libNmeIAP.so` (`NmeIAP2Interfaces.cpp`) decides it outright.
`OnListUpdate` iterates the top-level parameter array; **each occurrence of param 1 calls
`OnRecentsListItem` once and appends exactly one entry** to a realloc-grown array. That function then
runs the generic iAP2 TLV walker directly on the group's value, and the ids it finds are the fields —
Index, RemoteID, DisplayName, Label, AddressBookID, Service, Type, UnixTimestamp, Duration,
Occurrences — at one level, matching our field map exactly. `OnAppDiscoveryUpdate` has the identical
shape for params 1 and 4. Confirmed across two independent builds (aarch64 CT5 and x86-64 reference),
same source, two toolchains.

The two-level reading is not merely unattested — it is **incompatible with the shipping decoder
working at all**: CINEMO enforces strict lengths (Index exactly 2 bytes, UnixTimestamp 8, Duration 4),
so entry-sized nested blobs would fail every check and a GM head unit would show an empty Recents list.

Apple's own parser vocabulary agrees: `accessoryd`'s strings top out at `Param ID` / `Subparam ID`
with **no third-level term anywhere** in the iOS 27 extract.

**Confirmed on the wire, 2026-07-29.** A live iPhone sent this accessory a `0x4171` whose body is:

```
40 40 00 74 41 71 | 00 6e 00 06 | 00 06 00 00 00 00 | 00 16 00 01 "+1 (907) 385-5769"
 \____ iAP2 ____/   \_ id 6 _/   \_ id 0 Index _/   \__ id 1 RemoteID __/
```

The entry's fields sit directly inside the `FavoritesList` group — no per-entry wrapper. The flat
encoding is no longer an inference. Archived at
`docs/ops/captures/2026-07-29_iphone_0x4171_listupdate.txt` and pinned as
`real_iphone_favorites_entry_parses`.

Scope, precisely: the frame is 126 bytes and the log caps its hex dump at 48, so what is archived is
the header plus the first two entry fields — 28 of the group's declared 106 bytes. The remaining 78
were never dumped. That prefix is what the encoding question turns on, but the fixture is the visible
prefix, not the whole frame, and the doc comment on the test says so.
The phone declared `FavoritesListCount = 1` and exactly one record was emitted; the pre-fix parser
emitted zero from this frame.

SpeedPlay does not implement these messages at all (`libcustomiap.so` is link-layer only). No capture
contained 0x4171 bytes when this was written; one does now (above). `0xAD01` remains uncaptured —
the app-list consent has never been granted, so it has only ever arrived empty. The conclusion rests on the spec archive,
the hardware-proven Identify layout, `now_playing`'s working one-level group parse, and CINEMO's
disassembly — three finder/corroborator agents reached it independently.

**Fixed 2026-07-29.** The inner `walk` is gone from both parsers; `walk` is a linear scanner that
emits every occurrence of a repeated id, so repetition comes from the outer walk. `device_update`
0x4E0E now parses both identifiers as utf8 under correct key names, and `media_library` dispatches on
message id because 0x4C01's single top-level param is a group while 0x4C04's are flat. The two tests
that baked the wrong shape into their fixtures were rewritten, and
`repeated_groups_are_flat_one_tlv_per_entry` now pins the correct shape in both directions.

---

### 3. Fixed this pass

- **32-bit `usize` overflow in `annexb_from_avcc`** (`forward.rs`). On armv7 a 4-byte NAL length of
  `0xFFFFFFFF` wraps `i + len` below `au.len()`, the guard passes, and `&au[4..3]` aborts the daemon
  under `panic = "abort"`. Confirmed by execution on the target; invisible on a 64-bit host, which is
  why the suite never caught it. Now `checked_add`, with a regression test. Not reachable in the
  deployed config — both launchers set `OCBM_FWD_ENC=1` and the unsafe dev script is not on the box —
  but it was one environment variable away.
- **`skip=` dropped tokens after a space.** `skip=power, destination` skipped only `power`, so an
  operator bisecting a rejected declaration would misattribute the next unrecoverable reject.
- **`media_library` declared `0x4C02` without `0x4C00`** — a Stop with no Start, the exact asymmetry
  iOS polices, leaving `0x4C01` unreachable.
- **Five host-app key gaps** (`appIcon.iconTransferId`, destination coordinates/address/source,
  `laneGuidance.laneStatus`, `mediaLibrary.revision`) — emitted by the box, read by nothing.
- **Stale comments in `features.rs` and `message.rs`** claiming `extended` was device-rejected and that
  iOS "has never named param 7". Both were true when written and refuted within hours by the sessions
  that followed. Left alone they would have sent the next session backwards.
  **Correction, 2026-07-29:** this entry was written before the fix was complete. Two of the comments
  survived in `message.rs` (the Extended-tier pin and the `rx-only` rationale) and were only removed
  after a later verification pass caught the discrepancy between this document and the tree. A review
  document asserting a fix that did not land is the same failure it exists to prevent.
- **Correction, 2026-08-01:** this entry originally claimed docs/ops/02_TESTING.md's grep string `key probe exhausted`
  "could never match" the emitted `EXHAUSTED`. That claim was itself wrong: docs/ops/02_TESTING.md:135 already quotes
  `key probe EXHAUSTED` (uppercase), which matches the string emitted at `session.rs:1142` verbatim.
  There is a separate, differently-worded lowercase message, `key probe exhausted with no match —
  closing the undecryptable connection`, at `session.rs:1170` — but docs/ops/02_TESTING.md never quoted that one, so
  there was no mismatch to fix.
- Stale md5s, test counts and line references across the docs.

---

### 3a. The RCS sink demotion — fixed, but not the way it was proposed

`datastream::send` dropped the sink on **any** write error, and `register` has exactly one call site
(`session.rs`, inside `if chan.is_none() && !probed`), which runs once per accepted RCS connection.
Nothing could ever re-register. The reader half kept working, so a single transient
`WouldBlock`/`TimedOut` — a full kernel send buffer during a 4K video burst on the same wireless link
— left the tunnel **one-way** for the rest of the session: every ACK, Identify step and subscribe
silently fell back to `POST /command`, which the phone does not route into its iAP2 stack. The same
silent-one-way shape as the original `'comm'`/`'cmnd'` blocker.

The reviewer's proposed fix — keep the sink on a transient error — would have been **wrong**.
`ControlChannel::encrypt_frame` calls `seal_one`, which advances the ChaCha write counter *before* the
write is attempted. Once a counter value is burned, those exact bytes are the only ones the peer can
decrypt at that position. "Keep the sink, drop this frame" leaves the phone permanently one frame
behind — a silently desynced channel, worse than the fallback it replaces.

Fixed instead by **retrying the already-encrypted bytes**: `write_all_retrying` loops on `write`,
tracking progress, tolerating `WouldBlock`/`TimedOut`/`Interrupted` for up to 6 s (3× the socket's own
write timeout). Either the bytes go out or the channel really is finished — and in that case the
socket is now `shutdown`, so the phone sees a clean close rather than a frame it can never parse.
Regression test `transient_write_stall_does_not_lose_bytes` drives a real socket pair with a slow
reader so the stall is a genuine kernel condition, and asserts every byte arrives in order.

---

### 4. Confirmed, deliberately not fixed

**Artwork duplicate-fragment corruption.** `Artwork::on_session2` accepts a duplicated fragment,
emits a corrupt JPEG and acknowledges Success. Two reviewers confirmed by execution — one captured the
emitted bytes: `artwork id=0x81 4 B = [ff, d8, ff, d8]`, two SOI markers, no EOI.

Not fixed, on the third reviewer's recommendation:

- The proposed fix was self-defeating. Clamping the append to the declared size makes the
  `len != size` rejection unreachable; the two halves cancel and you ship "truncate and emit" while
  believing you shipped "drop and don't acknowledge".
- Truncation salvages nothing: completion is evaluated after every fragment, so an over-length buffer
  only arises when the boundary-crossing fragment overshoots — the excess is inserted mid-stream, not
  appended after EOI. `&buf[..size]` is corrupt from the duplication point either way.
- The proven wireless path negotiates Zero-Ack and so cannot retransmit. All four transfers on record
  are byte-exact. When it does fire the delta is a garbled cover rather than a missing one, both
  self-healing on the next track.
- `len != size` is the only branch capable of dropping an image that renders today.

Taken instead: an explicit `[art] OVERLONG …` line so the condition is unmistakable in a capture
rather than two numbers to compare. The root cause belongs in the link layer — `link::parse` has no
duplicate-sequence suppression, so under the retransmitting SYN profiles any re-sent frame is
re-dispatched. Artwork is simply the one payload where that is not idempotent.

---

### 5. Refuted

- **"Short frames bypass authentication."** The `body_size >= 16` gate is a byte-faithful port of
  Apple's R14G17 `AirPlayReceiverSessionScreen.c`, nonce-advance placement included. Rejecting instead
  would deviate from the normative source. The worst case is ≤11 bounds-checked bytes reaching a
  decoder on a path `OCBM_FWD_ENC=1` makes unreachable.
- **"A peer that has never paired can reach the parser."** The dataPort listener does not exist until
  after pair-verify on the encrypted control channel.

---

### 6. Test suite gives false assurance in specific, named places

Verified by mutation — each change below left the whole suite green:

- `EMITTED_KEYS` is an allowlist, not a contract. The host reads 69 keys; the list covers 27. *(**CORRECTED 2026-08-16:** `EMITTED_KEYS` is now 36 and `MetadataWindow.swift` reads 76 distinct keys — §9 of this doc records the additions, §6 was never restated. The finding itself, that an allowlist is not a contract, still stands.)* Renaming
  `currentRoadName` and `destinationName` in `route_guidance` blanks the Navigation pane and passes.
- `host_app_reads_every_emitted_key` scans the whole Swift file unscoped, so `("voiceOver","enabled")`
  is satisfied by an unrelated telephony display literal. It reports a contract satisfied that is not.
- ~~`proven_tier_reproduces_the_wired_and_tunnel_baselines` compares `SENT_MSG_IDS` to itself~~ —
  FIXED in §8: literal id lists plus an anchor to the captured 290-byte accepted Identify. Appending `0x4157` to the wired floor — growth on a hardware-proven surface —
  passes all tests.
- ~~`datastream::outbound_message_type_is_cmnd_not_comm` asserts the constant against itself~~ —
  FIXED in §8: the expected value is reconstructed from the two ARM64 immediates quoted in the module
  docs, so falsifying it means editing quoted disassembly. Changing `MSGTYPE_CMND` to `b"cmdn"` passes, reinstating the
  exact shipped blocker.
- `skip_list_narrows_the_declaration` never calls `active()` or `file_setting()`; it asserts that
  `Iterator::filter` works. The field lever it nominally covers has no real coverage.
- `stream.rs` encrypt/decrypt are exact inverses tested only for determinism, so the HKDF salt, the
  info labels and the nonce layout are all unpinned.

---

### 7. Outside the review's scope, for the owner

The box's AP uses the default PSK `12345678` in `/etc/hostapd.conf`, with no AP isolation and no
`iptables` binary present. Busybox `telnetd` listens on `:23` with no authentication, alongside
dropbear with a blank root password. Anyone positioned to reach a session's dataPort already has an
unauthenticated root shell.


---

### 8. Follow-up pass, 2026-07-29 — six proposals verified, three refuted

> **⚠️ THE `/tmp/carplay_metadata` LEVER IS NOT THE CANONICAL CONTROL.** Per docs/carplay/04_CAPABILITIES_AND_CONFIG.md the levers are
> interim, box-side scaffolding pending migration to app-pushed config — which also removes the
> orphaned-rider footgun class "The live hazard" below documents, since the app validates before
> pushing. The verified/refuted proposals and the landed fixes themselves stand. Full reasoning:
> [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-48-1`.

Each proposed fix was put to a dedicated verifier told that refuting it was equally valuable. One
survived intact, two were confirmed but materially improved, three were wrong — two of those would
have made things worse.

#### Landed

| # | Change | Notes |
|---|---|---|
| 1 | `Trigger` enum replaces `start`/`stop`/`extra_sent` | `stop: None` is now unwritable; the exception must be spelled `UnpairedStart { device_evidence }` |
| 2 | `active()` drops riders whose owner was skipped | closes a LIVE hazard, below |
| 3 | Policy + skip list resolved ONCE per process | the Identify and the subscribes can no longer disagree |
| 4 | `av_streams` keyed on `(type, channelID)` | `channelID`, not `streamID` — the latter is fresh per SETUP and would break re-SETUP superseding |
| 5 | Phase-1 SETUP made idempotent | matches `_ControlSetup`'s `controlSetup` guard; subsumes the Arc fix |
| 6 | Catalog-derived Start/Stop enforcement | derives the Stop from `spec::name_of`, so a WRONG Stop is caught too |
| 7 | `mfi_retry` no longer retries a lock timeout | `MfiError::LockBusy` vs `Chip`; 30 s → 10 s |
| 8 | `sign` poll bounded by wall clock | `for _ in 0..200` was ~7.1 s under NAK, not the ~2.1 s its comment claimed |
| 9 | `ocbmd` now takes the MFi lock | it was the fifth chip user and took none at all |
| 10 | Event capture truncates per session | NOT gated — see below |
| 11 | Test hardening | `'cmnd'` derived from the disassembly immediates; params 6/7 anchored to the captured 290-byte Identify; `REQUIRED_IDENT_PARAMS` pinned |
| 12 | `tools/run_tests.sh` | the root `cargo test` never ran most of this project's tests |

#### The live hazard

`lane_guidance` declares `0x5204` in param 7 but has no subscribe — it rides `route_guidance`, a
dependency recorded only in a comment. `echo "extended skip=route_guidance" > /tmp/carplay_metadata`
therefore declared an update whose trigger was absent from param 6: exactly
`OptionalMsgNotValidWithoutRequiredMsgs`, an unrecoverable `0x1D03` — produced by the very lever an
operator reaches for to narrow a *failing* declaration, and attributed to the wrong feature.
`Trigger::RidesOn` makes the dependency real and `active()` now drops orphaned riders.

#### Refuted, and why it matters that they were

- **Gating the event capture.** There *is* a 4 MiB cap; the file is 224 bytes over a full session
  because the event channel receives almost nothing inbound; and `docs/wireless/00_WIRELESS_CARPLAY.md`/`docs/wireless/00_WIRELESS_CARPLAY.md` both instruct the
  operator that it needs no env var. The severity claim was wrong by three to four orders of
  magnitude, and the fix would have broken two documented procedures and conflated two wire formats.
  Truncate-per-session was taken instead — it also automates a step `docs/wireless/00_WIRELESS_CARPLAY.md` does by hand.
- **Hoisting the MFi lock outside the retry loop.** The real worst case is ~51 s, not 30 s. The
  proposal delivers ~31 s while converting ~21 s of lock-*wait* into lock-*held* time, breaking the
  invariant the 10 s ceilings were sized against and spuriously failing `airplayd`'s own
  `LocalMfiSigner` — i.e. no session at all. It also cannot be written: `MfiLock` is private and
  `receiver` is `#![forbid(unsafe_code)]`.
- **Deferring the `stream_flag` fix.** The reachability argument held, the cost argument did not:
  `session.rs` has an in-file `mod tests` with `AvSession::new()`, so the two-channel case is a plain
  unit test, and on every path any capture shows the change is an identity transform.

#### Still open

- The structural fix for multi-second blocking I2C under `SESSION`: compute the action, drop the
  guard, run the MFi op, re-acquire, re-validate. Needs session re-validation logic on the path where
  every session lives or dies — a separate, separately-tested change.
- The event capture watches the `POST /command` carrier, which docs/carplay/05_METADATA_AND_CONTROLS.md refuted. It monitors a channel
  now proven silent; moving it to the DataStream path is a product decision.
- `docs/carplay/05_METADATA_AND_CONTROLS.md` §5.1 and §5.6 claimed the declaration rules were "structural in `features.rs`". Rules 1
  and 2 now are. Rule 3 is asserted by test. Rule 4 (consent) is not representable as a protocol
  invariant and remains unencoded.


---

### 9. Second pass, 2026-07-29 evening — six verifiers on the pending changes

Three of the four pending changes were verified correct; each verifier also found something real.

| Change | Verdict | What the verifier caught |
|---|---|---|
| Unify the two `:9004` writers | CORRECT, framing byte-identical, no deadlock | `reset_sink()` at teardown rested on a false premise — ocbmd's producer slot is CONNECTION-lifetime, not session-lifetime, so a held socket stays valid and the forced reconnect could discard an unread frame tail. Removed. Three comments in `session.rs` still described `META_SINK` carrying command plists on the control thread; corrected. |
| `callListStatus` scalars | CORRECT — ids 0/2/5/7 verified against Apple's dump, disjoint from the group ids, all three captured bodies decode as expected | `EMITTED_KEYS` — the cross-language contract that exists to fail on a rename — had NO entries for `callListStatus`, `recentCall` or `favorite`. They worked by coincidence. Added, which immediately exposed that the emitter half never exercised `list_update` at all. |
| Host app | Compiles, main-actor isolation compiler-proven under Swift 6 | The `(first)` marker was INVISIBLE: `os_log`'s `.auto` privacy is public for scalars but private for strings, so the unannotated ternary rendered as `<private>` and stamped that onto every sampled line. Also, routing status into `favorites` inflated the badge (5 for one favorite) and permanently suppressed the absence note. Split into its own `callLists` dict, rendered in both panes. |
| Docs vs code | — | docs/ops/05_AUDITS.md §3 claimed a fix that had not landed; docs/ops/02_TESTING.md carried two grep strings that can never match; docs/carplay/05_METADATA_AND_CONTROLS.md §5.6 overclaimed "structural" for all four rules; the "read at Identify time" lever description was wrong after policy caching. All corrected above. |

#### The seam-eviction bug, recorded

`session.rs` opened a SECOND TCP connection to `127.0.0.1:9004` for inbound `/command` plists while
`metadata.rs` held its own. ocbmd keeps one producer per channel
(`av_conns.retain(|(_, c)| *c != ch)`), so the two connections inside the same `airplayd` process
mutually evicted each other — every command plist killed the JSON sink and vice versa. Observed live
on 2026-07-29 as three `[meta] seam write failed — reconnecting` cycles, each exactly three
`modesChanged` forwards after the previous reconnect. The JSON side logged its losses; the command
side discarded `forward_to_sink`'s return, so its drops were silent. Both now share one connection.

Accepted trade-off, now documented in the code: the RTSP control thread is coupled to the metadata
plane through that one mutex, so a control command can wait behind an artwork write. Bounded at two
contending threads, against certain observed loss.

#### `0x5204` confirmed via `RidesOn`

The 2026-07-29 session delivered `0x5204 LaneGuidanceInformation` x12 alongside `0x5201` x574 — the
first live confirmation that a subscribe-less `Trigger::RidesOn` feature receives data. Lane guidance
has no `Start*` of its own; it is declared in param 7 and rides `route_guidance`'s subscribe.

---

## HISTORICAL — first code audit

<!-- absorbed: ../ops/05_AUDITS.md -->

Read-only audit of ALL Rust + Swift code (18.5k lines) by 12 agents, one per non-overlapping unit.
Every finding is grounded in file:line and marked **CONFIRMED** (traced, certain) or **SUSPECTED**
(needs a further check / unreachable-today). Nothing was modified.

Roll-up: **Critical 0 · High 5 · Medium ~13 · Low ~35 · plus large dead-code inventory.**
No memory-safety defect (no leak, double-free, data race) survived verification. No panic-on-untrusted
-input found. All crypto/counter/framing paths (incl. the recent nav-gate edits) verified CORRECT.

---

### HIGH

#### H1 — Video cut-out ROOT CAUSE: shared A/V queue couples the streams  `ocbmd/main.rs:998-1006` [CONFIRMED coupling / SUSPECTED exact-fps]
The read-gate `if matches!(ch, CH_VIDEO|CH_ALT_VIDEO) && !out_av.is_empty() { continue }` keys on a
**single shared FIFO** (`out_av`) carrying CH_VIDEO + CH_ALT_VIDEO + CH_MEDIA_AUDIO + CH_ALT_AUDIO.
So the main 4K video seam is not read whenever ANY byte from the cluster or audio is still queued. The
low-bitrate cluster (:9005) + audio keep `out_av` almost continuously non-empty → 4K starves (~2fps,
8s stalls). Backpressure was designed for audio-vs-video, never video-vs-video. **This is the live
"video cuts in and out" bug.** Fix: per-stream queues + per-stream gates + fair drain. Compounded by
**M-a** below (`:1077` av_conns index-shift can misroute bytes onto the wrong channel during :9005
reconnects the toggle triggers).

#### H2 — `setupDevice` has no re-entry guard  `AppDelegate.swift:358` [SUSPECTED]
Overwrites transport/decoders/client/bridge/coordinator with no `endSession()` first. Normal flows are
serialized (detach precedes attach), but a duplicate `usbDeviceDidConnect` would abandon the previous
`OCBMClient` (its bulk-read loop still running) → leak + two clients on the USB pipe.

#### H3–H5 — Three entire subsystems are DEAD (never instantiated) [all CONFIRMED]
- **MicCapture.swift** — Siri/phone **mic uplink never runs**. `micCapture` is only ever `nil`;
  `onPCMData` never assigned. On Siri/call the mic never opens → phone never hears the user.
- **NowPlayingManager.swift** — no Control Center / `MPNowPlayingInfoCenter` / media-key handling.
- **CallManager.swift** — no incoming-call notifications / Accept-Reject / caller-ID / call-duration.
These are the legacy carlink-USB path, superseded by OCBM + MetadataStore. Flagged HIGH for magnitude
(whole advertised subsystems inert), not memory-safety. **Decision needed: intentionally retired, or to
be re-wired?** (Live call state IS shown in the MetadataStore Phone pane; Now-Playing is not surfaced.)

---

### MEDIUM

- **M-a** `ocbmd/main.rs:1077-1087` [CONFIRMED shift] — `av_conns.retain()+push()` during the same
  dispatch pass shifts pre-captured `AvConn(idx)` → same-wake re-accept can frame one stream's bytes
  onto the wrong OCBM channel (e.g. 4K bytes as CH_ALT_VIDEO). Overlaps the :9005 reconnect path.
- **M-b** `ocbmd/main.rs:529` [CONFIRMED] — `drain_q` uses `q.drain(0..w)` (O(n) front-shift) → O(n²)
  draining a 4K-frame `out_av` on the ARM box. (rx path was fixed with a cursor; tx wasn't.)
- **M-c** `airplayd/main.rs:380-430` [CONFIRMED] — `CARPLAY_ALT_W/H` set but **never cleared** on any
  path (else/parse-fail/no-config). Stale cluster dims from a prior connection leak into the next →
  wrong cluster resolution advertised to iOS.
- **M-d** `AppDelegate.swift` [CONFIRMED] — **window-title state machine broken** ("stuck at
  Disconnected"): only the dead `adapterDidDetectPhoneType` sets the phone-type title, so over OCBM it
  never advances; a replug leaves "CarLink — Disconnected". Live user-visible bug.
- **M-e** `session.rs:254` [SUSPECTED] — the type-111 default-OFF **`stopUI` is a no-op at initial
  SETUP** (EVENT channel isn't wired until RECORD, which runs after SETUP) → returns false, never sent.
  iOS keeps encoding the cluster; only the box `nav_forward()` gate drops the frames. Comment claiming
  "iOS sends no frames" is wrong/misleading. (Confirms stopUI never stopped the encoder.)
- **M-f** `SettingsWindow.swift:162-183` [CONFIRMED] — `nightMode` + `rightHandDrive` **not persisted**
  in `save()`; revert to false on next launch (silent data loss). Bounded today (box doesn't parse them).
- **M-g** `MetadataWindow.swift:230` [CONFIRMED] — `MetadataStore.resetSession()` **never called**;
  `AppDelegate.resetSession` doesn't clear the store → stale metadata/artwork across a session reset.
- **M-h** `CallManager.handleCallStatusJSON` [CONFIRMED] — doubly unreachable (`onCallStatus` never
  assigned + no external caller). (Moot while CallManager is dead — H5.)
- **M-i** `CarPlayView.swift:523-531` [CONFIRMED] — **Android-Auto** crop-mode vertical touch mapping
  ignores the symmetric top crop (taps land too high, error grows toward the bottom). AA-only; the
  primary CarPlay touch path is correct.
- **M-j** `ProtocolSessionRecorder.swift` [CONFIRMED] — the binary session recorder is **never armed**
  in the shipping app (start/stopRecording only in tests). Dormant infra.
- **M-k** `uplink.rs:95-115` [SUSPECTED] — stereo PCM uplink RTP timestamp advances 2× too fast
  (interleaved count instead of samples-per-channel). Unreachable today (voice uplink is mono).

---

### LOW — behavioral / robustness (selected; ~15 total)
- `airplayd/main.rs` — `NAV_FORWARD` not reset on session teardown → a `CMD_NAV_START` then disconnect
  leaves the next session forwarding the cluster from the start [CONFIRMED]. `INPUT_NAV` sent to uid-3
  unconditionally regardless of `CARPLAY_DPAD` → silent no-op when D-pad off [CONFIRMED]. Media-btn
  index not range-validated [CONFIRMED].
- `ocbmd/main.rs` — `av_backpressured` set once never reset; `forward_input` claims non-blocking but
  the socket is blocking (can stall the poll loop); `CT_SRC` srcbench blocks the loop up to 30s; `eth`
  AF_PACKET socket / CH_IP `conns` leak across sessions [all CONFIRMED].
- `metadata.rs:315` — RouteGuidance param id 24 mislabeled (maps ArrivalBatteryLevel to
  "destTimeZoneOffsetMin"; real tz offset is id 21, never parsed) [CONFIRMED].
- `MetadataWindow.swift:124/356/386` — array-index by a negative JSON value would trap (box-generated
  feed, low risk) [SUSPECTED]. Metadata deltas hop to main via fresh unstructured Tasks → possible
  out-of-order apply across chunks [SUSPECTED].
- `USBTransport.swift:201` — idle-timeout path doesn't reset `consecutiveErrors` (disconnect slightly
  eager) [CONFIRMED]. `FileLogger` — no in-session size/rotation cap [CONFIRMED].
- Validator disagreement `vehicle_config` vs `info.rs` on OOB insets (feature flips on but emits
  full-bleed) [SUSPECTED]. `net.rs:37` pre-A/V idle tears down after one read-timeout [SUSPECTED].

### LOW — DEAD CODE inventory (all CONFIRMED unless noted)
**Whole files / subsystems dead (legacy carlink-USB, superseded by OCBM):**
`AdapterProtocol.swift`, `MessageParser.swift`, `MessageSerializer.swift` (+ its view/safe-area +
naviScreenInfo binary builders), `SessionTokenDecryptor` (only the dead adapter calls it),
`NavWindowController`+`NavVideoView` (~145 lines, removed 0x2C feature), `MicCapture`,
`NowPlayingManager`, `CallManager`, the whole `AdapterProtocolDelegate` extension in AppDelegate
(~200 lines) + orphaned helpers (`resetProjectionState`, `hotAdapter`/hot-ref fast path, `VoiceMode`),
`AppDelegate.vehicleConfigYAML(width:height:)` (old hardcoded template, has a wrong `enablesDPad` key).

**Dead symbols/fields:** `send_take_main_audio` (events.rs); ocbm-proto consts `MBTN_*`/`ACODEC_*`/
`ATYPE_*` (14, values hand-duplicated → drift risk); `OCBM.sevHostGone`, `cmdRequestUI`,
`cmdRequestSiri` (OCBMFraming); `videoCounter`/`altVideoCounter`, `altVideoOK`/`altVideoFail`,
`OCBMAudioStreamFormat.bits` (OCBMAVDecrypt, write-only); `FieldInfo["name"]`/`["altResolution"]`
(SettingsWindow, never rendered); `NavSnapshot.etaEpoch`, `Maneuver.maneuverType`,
`MediaMetadata.MediaAPPName` (MetadataWindow/NowPlaying); vehicle_config `HidConfig.knob_support`,
`AltVideoStream.max_fps`, `ViewAreaEntry.view_area`/`AreaRect` (parsed but never applied); stream.rs
non-`_aad` crypto + `RtpHeader` (test-only); iap2d `group()` (test-only); Communications 0x4158 + List
0x4171 subscribe/parse paths (unreachable — ids not in RCV_MSG_IDS).

**Stale comments (code correct):** ocbm-proto `INPUT_MEDIA_BTN` (claims no 3rd HID device; there is a
uid-3 D-pad); ControlsWindow header (nav on uid2 → actually uid3); CH_METADATA comments
(META_CMD-only → actually 3 markers); `events.rs` setLimitedUI doc misattached to NAV_FORWARD.

---

### VERIFIED CORRECT (checked because flagged; no defect)
- The recent **crypto/counter/nav-gate** change: `enc_seq` stays 1:1 with the iPhone per-VideoFrame
  nonce across the gated span; host resyncs on resume via SEAM_MAGIC + fresh keyframe. (Both the box
  and host agents confirmed independently.)
- The **safe-area/viewAreas** change: `view_areas` validation ⊆ panel + fallback; `safe_area_inset`
  full-frame-vs-inset; `apply` ordering; caller env-lever wiring. 27 receiver tests pass.
- The Settings **YAML `va()` nesting** for BOTH main + alt streams (the prior scalar-fold bug is fixed);
  **forceCustom** resolution state machine; reentrancy guard; **every ControlsBridge opcode**.
- iAP2 parsing is uniformly **bounds-checked** (no untrusted-input panic). IOKit/USB lifecycle balanced
  (no leak/double-release). Audio engine graph single-threaded; ducking generation/deadline correct.
- OCBM framing constants match ocbm-proto end-to-end; dual-lane (main/alt) fully separated.

---

### FIXES APPLIED (2026-07-12, post-audit)

All box daemons cross-build clean (0 warnings); receiver 25 + ocbm-proto 9 + iap2d 5 + ocbmd 2 tests
pass; Swift app builds. Fixes, grounded in the audit:

**Video root cause + ocbmd (H1, M-a, M-b, LOW):**
- Split the shared `out_av` FIFO into per-stream queues (`out_video`/`out_alt_video`/`out_audio`) with a
  new cursor-based `OutQueue` (no O(n²) front-shift). Read-gate now keys each video seam on ITS OWN
  backlog, audio never gated + drained first. → the cluster can no longer starve the main 4K stream.
- Deferred `av_conns` accept to after the dispatch pass (no mid-pass index shift → no misrouting).
- `av_backpressured` now resets on recovery; `eth` socket closed on session teardown.

**airplayd / receiver (M-c, M-e, LOWs):**
- `CARPLAY_ALT_W/H` cleared on every config path (no stale cluster dims).
- `NAV_FORWARD` reset in `events::clear()` (no cross-session cluster-forward leak).
- `INPUT_NAV` gated on `CARPLAY_DPAD`; `INPUT_MEDIA_BTN` index range-checked (1..=5).
- Removed the misleading no-op `stopUI` at type-111 SETUP (the box forward-gate is the real mechanism).
- `clear_sinks` now resets `META_SINK`; `setLimitedUI` doc de-tangled from `NAV_FORWARD`.
- Removed dead `send_take_main_audio`; `iap2d::group()` is now `#[cfg(test)]`.
- `metadata.rs`: fixed RouteGuidance param IDs — id 24 = `arrivalBatteryLevel` (was mislabeled), added
  id 21 = signed `destTimeZoneOffsetMin`.
- Corrected stale/misleading `ocbm-proto` comments (INPUT_MEDIA_BTN 3rd-HID-device claim; CMD_REQUEST_UI).

**Swift host (H2, M-d, M-f, M-g, M-i, LOWs):**
- `setupDevice` re-entry guard (ends a live session before re-setup — no leaked OCBMClient).
- Window title driven by the live OCBM streaming signal (fixes "stuck at Disconnected"); `h264Decoder`
  now nil'd on teardown (symmetric).
- `endSession` clears `MetadataStore` (no stale metadata across a session reset).
- `nightMode`/`rightHandDrive` now persisted in `save()`.
- Android-Auto crop-mode touch mapping unified with the standard path (accounts for the crop offset).
- Negative-index guards on media/call/direction label lookups.
- USBTransport resets the error streak on idle timeouts; AltVideoWindow idle timer stops while hidden.
- Removed dead `NavWindowController`/`NavVideoView` (145 lines), `vehicleConfigYAML`, the dead
  `cmdRequestUI`/`cmdRequestSiri` host constants; fixed stale ControlsWindow / CH_METADATA-adjacent /
  SettingsWindow (`altResolution` key now wired, dead `name` key removed) comments.

**DEFERRED — pending decisions/hardware:**
- **Legacy AdapterProtocol subsystem** (`AdapterProtocol.swift`, `MessageParser.swift`,
  `MessageSerializer.swift`, the `AdapterProtocolDelegate` extension + `adapter`/hot-ref properties):
  ~600 lines of confirmed-dead but deeply-interconnected code. Removal is behaviorally safe (dead) but
  large; recommend a dedicated pass.
- **MicCapture / NowPlayingManager / CallManager** (H3–H5): dead, but they are FEATURES (Siri mic
  uplink, Control Center, call notifications). **Retire (delete) or re-wire?** — user decision.
- **Box deploy of the video fix**: staged (`ocbmd a2a770c8`, `airplayd e7ae805`) but NOT deployed — the
  UART serial adapter was disconnected mid-session. Deploy + on-hardware video-stability test pending.
- Minor: FileLogger in-session size rotation; stereo-PCM uplink timestamp (unreachable today);
  OCBMAVDecrypt write-only `videoCounter`/`altVideoCounter`/`bits` fields.
