# CORRECTIONS LEDGER — why documents changed

> **STATUS:** HISTORICAL RECORD · **`R-<doc>-<n>` ids key to the PRE-CONSOLIDATION doc numbers**
> (docs/carplay/00_ARCHITECTURE.md, flat). On 2026-08-31 the corpus was consolidated into `carplay/`, `wireless/`,
> `host/` and `ops/`; those originals live in git history and in the 2026-08-31 backup. This ledger
> is kept for provenance — *new* corrections are made in place in the owning document, not appended
> here, and the split-signal scheme it describes below is retired. `tools/docs_status_check.py`, which
> enforced that scheme, is replaced by `tools/docs_check.py` (per-category cap, STATUS lines, links).

This file holds the **reasoning** behind every correction, retraction and supersession applied
to a document in `docs/`. The documents themselves keep only a short signal that points here.

## Why the split

A correction block does two jobs with opposite retrieval needs:

- a **read-time** signal — "stop, this page is stale" — which must be impossible to miss while
  reading the page, and
- an **archival** record — "here is what we believed, why it was wrong, and what replaced it" —
  which is looked up rarely, deliberately, and usually long afterwards.

Keeping both in the document served the second badly and eventually broke the first. A
blockquote-block scan on 2026-08-16 measured **875 lines of correction apparatus in 76 blocks
across 47 documents**, and several files were past the point of linear readability — docs/carplay/03_SDK_GROUND_TRUTH.md
was ~22% correction by volume. (An earlier estimate of ~816 lines / 5.3% with per-file
percentages for docs/ops/04_OPEN_ITEMS.md and docs/wireless/01_BT_AND_RADIO.md came from a differently-drawn measure that also counted
inline strikethrough; the block scan above is the reproducible one, via the worklist query in
this file's commit history.) Worse, the in-doc blocks became a drift surface of their own — docs/wireless/00_WIRELESS_CARPLAY.md's banner asserted a thing
docs/carplay/03_SDK_GROUND_TRUTH.md had already retracted, and the 2026-08-16 QC produced three corrections-of-corrections
in a single session, one of which reversed a "fix" whose original text had been right.

Git already records who changed what and when, better than a hand-written date ever will. So
the reasoning moves here and the signal stays there.

**What must NOT move: the read-time signal.** The rule every migrated document has to satisfy:
a `STATUS:` line at the top, AND a one-line marker at every corrected section naming its `R-`
entry. Losing that is the one failure mode this reorganisation must not cause — docs/carplay/05_METADATA_AND_CONTROLS.md §7
records docs/wireless/00_WIRELESS_CARPLAY.md naming a bug correctly on 2026-07-17 and the finding going unactioned for eight
days, because nothing on the page said "act on this".

This is stated as a rule, not as a description: the migration is in flight, and
`tools/docs_status_check.py` reports how far it has got. A section marker that merely says "the
material below still stands" is not sufficient where only *part* of it stands — enumerate what
survives, or name the exceptions.

## How entries are keyed

Each entry is identified by a stable ID — `R-<doc>-<n>`, e.g. `R-49-2` — which is what the
document cites. Grep the ID to jump between the two.

`<doc>` is the document number where that is unambiguous, and alphanumeric where it is not.
Two cases exist: `docs/20` is a **filename collision** — two unrelated documents share the
number — so they key as `R-20M-n` (`../carplay/05_METADATA_AND_CONTROLS.md`) and `R-20W-n`
(`../wireless/00_WIRELESS_CARPLAY.md`); and two documents have no number at all, keying as
`R-PLAN-n` and `R-HANDOFF-n`. Renumbering the docs/20 collision is Phase 3.2 of
`../ops/04_OPEN_ITEMS.md`; until that lands, the `M`/`W` suffixes are what
disambiguates, and they stay valid afterwards.

A citation may name a range — `` `R-49-2` ``–`` `R-49-6` ``. The checker expands ranges and
requires every interior ID to exist, so a range cannot silently exempt its middle.

Each entry names the commit that landed the change as **commit subject + short SHA**. The
subject is primary and the SHA is the fast path, because *subjects survive a history rewrite and
SHAs do not*: this repo has already had one rewrite (2026-08-16, stripping session-link
trailers), which changed every SHA above the rewrite point while leaving subjects untouched. If
a SHA below does not resolve, search by subject:

    git log --all --format='%h %s' | grep -F '<subject>'

A SHA in backticks MUST resolve — `tools/docs_status_check.py` fails the run otherwise. A SHA
known to be dead (squashed away, or rewritten out) is written bare, without backticks, together
with whatever replaced it. That way the checker stays strict without forcing the record to
pretend a lost commit never existed.

## Conventions

- **Append-only.** A later reversal is a NEW entry that references the earlier one; entries are
  not edited to hide that we changed our minds. The sequence *is* the record.
- Entries are ordered by document number, then by ID.
- `Verdict` uses one of: `REFUTED`, `PARTIALLY SUPERSEDED`, `SUPERSEDED`, `CORRECTED`,
  `STALE`, `SHIPPED`, `REVERSED`.
- Text moved here from a document is preserved as it was written, not paraphrased. Where a
  claim in the moved text has since been re-verified or re-dated, that is recorded as its own
  entry rather than by rewriting the original.
- **Do not rewrite code anchors while moving text.** Move first, verify anchors as a separate
  pass. The pilot migration (docs/carplay/03_SDK_GROUND_TRUTH.md) ignored this and fabricated three anchors out of about
  sixteen rewritten — a directory that did not exist, a symbol absent from the file named, and
  a cross-reference to a section that did not exist. All three read as plausible. `R-49-6`
  exists because a fourth error was carried in the same way. `tools/docs_status_check.py` now
  checks that every backticked repo path resolves, but it cannot check that a *symbol* anchor
  points at the right thing — only a human or a verifier reading the code can.
- **Two documented deviations from verbatim, both formatting-only.** (1) Two moved blocks opened
  with their own markdown heading — an `## ⚠️ CORRECTED 2026-07-30 …` in `R-20M-2` and a
  `# RETRACTION REVERSED — 2026-07-25 …` in `R-32-6`. Dropped into this file unchanged they would
  have registered as ledger-level section breaks and fragmented it, so both were demoted to bold.
  No characters of the text itself were changed. (2) Four commit hashes in `R-55-1`/`R-55-2` had
  their backticks stripped, per the dead-SHA convention above; the hashes themselves are untouched.
  Nothing else in any entry was reformatted.
- `Landed:` names the commit that made the *substantive* change — usually the code fix, not the
  commit that wrote the prose. Where a correction is documentation-only, the docs commit is the
  substantive one and is named. If you need the commit that authored an entry's wording, use
  `git log -S` on a distinctive phrase from it.

---

## docs/carplay/00_ARCHITECTURE.md — Architecture

### R-01-1 · The committed model supersedes the "Model A passthrough vs Model B decode-on-adapter" framing

- **Verdict:** SUPERSEDED
- **Landed:** `Baseline before 2026-07-25 QC remediation batch` (`18ba44b`, 2026-07-25)
- **Scope:** §"The committed model: split by configurability (docs/carplay/04_CAPABILITIES_AND_CONFIG.md), not by transport"

This supersedes the earlier "Model A passthrough vs Model B decode-on-adapter" framing. See the
repo `README.md` §Architecture for the canonical statement; this is the architecture-doc detail.

## docs/carplay/01_OCBM_PROTOCOL.md — OCBM wire specification

### R-02-1 · Holding the `CT_RADIO` inhibit across a same-host resubscribe: implemented, then reverted

- **Verdict:** REVERTED — the unconditional clear stands
- **Landed:** uncommitted at time of writing (2026-08-27 lifecycle/health work)
- **Scope:** §"`CT_RADIO` and `CT_SUBSCRIBE`: the inhibit clear is unconditional", and the `0x16` row

**The idea.** A host holding a live wireless CarPlay session over Wi-Fi loses the box (USB bumped, box
power-cycled) and reattaches. CarPlay does not depend on Bluetooth once it is streaming, so bringing
the BT stack up underneath it is pure disturbance. Make the `CT_SUBSCRIBE` handler clear
`RADIO_OFF_FLAG` only when `cfg_changed || replaced`, and a same-host/same-cfg reattach then keeps its
inhibit.

**Why it was reverted.** Both halves failed, in opposite directions:

- *Inert where it was wanted.* Losing the box stops heartbeats, and after `HEARTBEAT_GRACE` (10 s)
  `go_idle` clears both `self.cfg` and the inhibit. A real host-side USB re-enumeration round trip —
  detach, attach intent, permission, claim, HELLO retries, MFi — takes far longer than 10 s, so the
  `CT_SUBSCRIBE` that eventually arrives always sees an empty cfg, reads as changed, and clears anyway.
  The carve-out never fired in the scenario it was written for.
- *Unsafe where it did fire.* The only window it actually covered was a quick Stop/Start by the same
  process with unchanged settings. There, holding the inhibit strands the box: `wireless_up()` returns
  early on the flag, so no BT phase is ever emitted, so the host arms no watchdog and never learns.
  The box sits with radios off and nothing to notice it.

**What replaced it.** The host owns the lever. A host wanting radios down after a reattach re-asserts
`CT_RADIO 0` itself, which does not depend on the box remembering state across a session boundary.
Correspondingly, an inhibit a host sets is an inhibit that host must release — the GM head-unit app
now ties its release to the session ending rather than to a timer.

**General lesson worth keeping:** the box deliberately forgets session state at `go_idle`, so any
"remember this across a reattach" design has to survive a 10 s heartbeat grace it almost certainly does
not. Cross-session intent belongs to the host.

### R-02-2 · `MfiError::LockBusy` conflated "lock held" with "lock unopenable", hiding a permanent failure as a transient one

- **Verdict:** CORRECTED
- **Landed:** uncommitted at time of writing (2026-08-27)
- **Scope:** `crates/vendor/mfi-i2c-local/src/lib.rs` (`MfiError`, `MfiLock::acquire`),
  `crates/vendor/receiver/src/iap_tunnel.rs` (`mfi_retry`), `ccpa/mfid/src/main.rs` (`status_for`)

`MfiLock::acquire()` returned `Option`, so both failure modes collapsed to `None` and every caller
mapped `None` to `LockBusy` — printing "another chip user holds /tmp/carplay_mfi.lock".

On the box that reading is usually right. On Android it never was: `/tmp` does not exist there at all
(verified as uid 2000, which is far more privileged than the app — `ls: /tmp: No such file or
directory`), so `open(O_CREAT)` fails `ENOENT` before any lock is attempted. There is no other chip
user and no contention. The receiver's AirPlay-tunnel iAP2 handshake therefore failed **every attempt,
deterministically**, and the resulting log line described a transient race — which is how it was read
across three evidence captures from 2026-07-22 onward, while the metadata/controls channel silently
never worked.

`acquire()` now returns `Result` and reports `LockUnavailable` distinctly, with the errno.

**The trap this is meant to stop.** "Lock path unopenable" invites repointing the lock somewhere
writable. That would not help: the next step is `/dev/i2c-1`, which is SELinux-blocked for an ordinary
Android app UID (gm_ccpa `evidence/03`) — and note the DAC bits are `crw-rw-rw-`, so checking the
node's permissions looks fine and tells you nothing. The path is not the problem; doing local chip
access on Android at all is. The fix is gm_ccpa `11_HARDENING_PLAN.md` T5.3 — route the tunnel's two
chip call sites through the `MfiSigner` the `ControlServer` already holds, which on Android is the OCBM
`CH_MFI` relay that `/auth-setup` uses successfully in the very same session.

**Wire compatibility:** `mfid` maps the new variant onto the existing `Status::LockBusy` rather than
spending a new discriminant. On the box the condition means a broken rootfs and should not occur; the
distinction is kept where it is diagnostic — in `MfiError`, which is what the in-process callers match.

**General lesson:** an error type that merges "cannot try" with "tried and lost" will be read as the
second, and the difference between permanent and transient is exactly what a reader needs.

### R-02-3 · The CTRL table was split by a blank line, `CT_PROJ_MODE` had no row, and the opcode range was stale

- **Verdict:** CORRECTED
- **Landed:** uncommitted at time of writing (2026-08-27 post-audit remediation, R6.7 step 35)
- **Scope:** §"CTRL message types", §"Self-describing streams"

Three pieces of drift, all from the same cause: `0x19` `CT_PROJ_MODE` shipped in `ocbm-proto` and
`ocbmd` and was written up in docs/androidauto/00_ARCHITECTURE.md and docs/androidauto/00_ARCHITECTURE.md, but never in the wire spec that is supposed to be
the single place a third implementation reads.

- A blank line sat between the `0x18` and `0x1A` rows, so a markdown renderer ended the table at
  `0x18` and left the `0x1A` `CT_BOX_HEALTH` row as an orphaned line of pipes. The gap is exactly
  where the missing `0x19` row belonged — the blank line is the fossil of the omission.
- The `0x19` row is now written from `crates/ocbm-proto/src/lib.rs` (`CT_PROJ_MODE`, `PM_*`) and
  docs/androidauto/02_ARBITRATION.md, including the reserved `PM_WIRELESS_AA`.
- "Self-describing streams" described the `CT_*` space as `0x01`-`0x18`; it is `0x01`-`0x1A`.

**5.13(d) was examined and dropped, not overlooked.** The audit also flagged the `CT_SETTIME` ack in
the `0x05` row as undocumented behaviour. It is not: `ocbmd` does send the ack exactly as the row
describes. Only the JNI lacks a handler for it, which is app behaviour, not doc drift, so the row
stands unchanged.

### R-02-4 · Envelope flags bit2 is no longer reserved — it is `F_REPLAY`

- **Verdict:** CORRECTED
- **Landed:** uncommitted at time of writing (2026-08-27 post-audit remediation, R6.4 step 22)
- **Scope:** §"Envelope (v1)"

`/tmp/bt_phase` was found to be a latch: nothing ever wrote an idle value back, so it meant "the
deepest handshake phase reached since boot", and `ocbmd` re-emitted it to every fresh subscriber —
producing "phone detected" windows of 47 s, 78 s, 133 s and 5 m 47 s with no phone anywhere. The latch
itself is fixed at its source (`carplay-wireless` now publishes `BTP_IDLE` on every session-end path
and at process start; the supervisor unlinks the file on `wireless_down`).

**Follow-up 2026-08-28 (found by test, `bt_driver::tests::session_end_publishes_the_idle_phase_so_the_mirror_cannot_latch`):**
the idle publish was written at the bottom of `bt_driver::run`'s loop and its comment claimed "every
session-end path funnels here". It did not: `run` has three early `return`s before the loop is
entered (detect-prelude write, initial-SYN write, `setsockopt`), and those are precisely the ones a
phone dropping RFCOMM during bring-up takes — so the latch survived exactly the case the fix was for.
`run` is now a thin wrapper that calls the session body and publishes `BTP_IDLE` on return, which is a
real single funnel. `run_active_session`'s own idle publish stays: it covers the preempt/go-quiet case
where `bt_driver::run` is never entered at all.

**General lesson:** "every path funnels here" is a claim about control flow, and a bottom-of-function
statement only funnels the paths that reach the bottom. Early returns are paths.

`F_REPLAY` is the defense-in-depth half, and it is what generalises the fix: it is set on ANY box→host
state-mirror frame the box emits because it held no prior value, so a host can tell a replayed state
from a change on all six mirrors (`bt_phase`, phone presence, pairing code, phone identity, box
health, projection mode) rather than only on the one that was caught lying.

**Why the predicate is "the box had no prior value" and not "the host just resubscribed."** The first
read after an `ocbmd` restart is also a first read, and a resubscribe marker would miss it. Both cases
are the same `None -> Some` transition in the daemon, which is what is actually tested.

Bit2 was documented reserved and is validated by no receiver on either end, so a receiver that does
not know the bit is unaffected and the two sides may adopt it in either order.

## docs/ops/01_RECOVERY.md — Recovery, CONSOLE mode, and install

### R-04-1 · `install_fhs.sh` is no longer the commissioning path — `ncm_base_install.sh` + `ocbm_install.sh` are

- **Verdict:** SUPERSEDED
- **Landed:** `Docs QC: batch-fix the remaining audit findings across 51 files` (`a08b0eb`, 2026-08-16)
- **Scope:** §"Persistent FHS install (`tools/install_fhs.sh`) — DONE (2026-07-10)"

**SUPERSEDED 2026-08-16 as the COMMISSIONING path — the FHS layout below still ships.** A unit is
now taken to the owned NCM base by `tools/ncm_base_install.sh` (preflight → full NOR backup →
owned boot path → cold test → strip → audit → cold test again), and OCBM is installed onto that
base by `tools/ocbm_install.sh` (place → verify → reboot → reversible `trial` behind a dead-man
timer → `finalize`). `ocbm_install.sh`'s `manifest()` is the authoritative file set: minimal is
`ocbmd` + `ocbm_boot.sh` + `run_ocbmd.sh`, and `--full` adds `iap2d`, `airplayd`, `rx-connect`,
`carplay-wireless`, `session_supervisor.sh`, `projection_up.sh`, `phone_reset.sh`,
`run_supervisor.sh` and the radio seam (`radio_detect.sh` / `radio_hal.sh` / `radio_ap_up.sh`,
see docs/wireless/01_BT_AND_RADIO.md). It installs the boot hook from **`ccpa/rootfs/script/ocbm_boot.sh`** — the copy
under `tools/` is a stale 34-line duplicate with no `/script/ocbm_trial` dead-man, and `finalize`
refuses to run against a box whose `ocbm_boot.sh` lacks it. `install_fhs.sh` remains useful as the
quick live-overlay refresh of an already-commissioned box.

## docs/ops/00_BUILD_AND_DEPLOY.md — Build toolchain & storage footprint

### R-05-1 · The component-footprint table is pre-build estimates, and its three C rows describe helpers that were never written

- **Verdict:** SUPERSEDED
- **Landed:** `Docs QC: batch-fix the remaining audit findings across 51 files` (`a08b0eb`, 2026-08-16)
- **Scope:** §"Component footprint (armv7-musl, release + size-opt + stripped, estimates)"

**SUPERSEDED 2026-08-16 — pre-build ESTIMATES, kept as the record.** The three C rows below describe
helpers that lived in the sibling `ncm_carplayd/ccpa/probes/` tree; **none of them was ever written for
this repo.** Every box daemon is Rust (`find ccpa -name '*.c'` returns nothing; the only C *binary* we
ship is the 45-line `accessory_init/iap_role_switch.c`, and the only other C in a shipped artifact is
`crates/vendor/eld-codec/csrc/eld_shim.c`, the libfdk-aac FFI shim compiled into `airplayd` — see
`../carplay/00_ARCHITECTURE.md` §Vendored assets). Real measured sizes are in the MEASURED / UPDATE blocks below;
read those, not this table.

### R-05-2 · Two of the three original recommendations did not survive contact with the build

- **Verdict:** SUPERSEDED
- **Landed:** `Docs QC: batch-fix the remaining audit findings across 51 files` (`a08b0eb`, 2026-08-16)
- **Scope:** §"Recommendation"

**SUPERSEDED 2026-08-16 — kept as the record of the original plan.** Two of the three
recommendations below did not survive contact with the build. (1) **"Keep the low-level glue in C"
was NOT taken:** no C MFi bridge, no C iAP2/radio glue and no C L3-NCM bridge was ever written for
this repo — the box daemons are all Rust; the only C *binary* we ship is the 45-line
`accessory_init/iap_role_switch.c`, and the only other C inside a shipped artifact is
`crates/vendor/eld-codec/csrc/eld_shim.c`, compiled into `airplayd` (see `../carplay/00_ARCHITECTURE.md`
§Vendored assets). (2) The "hard measured number… is the first thing to produce" **was produced** —
on 2026-07-10 and re-measured 2026-08-16, in the MEASURED / UPDATE blocks earlier in this file:
`receiver_core`'s pairing/session path cross-compiles clean and `airplayd` is ~1.71 MiB unpacked,
not the feared 4–8 MB. What DID hold: the Rust size profile, UPX packing, and the box scope of
pairing + key-derivation + RTSP relay with no decode.

## docs/ops/04_OPEN_ITEMS.md — Roadmap

### R-06-1 · The "Next critical path" list is a mid-2026-07 snapshot; items 1–10 are all complete

- **Verdict:** SUPERSEDED
- **Landed:** `docs: accuracy audit — fix ~30 wrong/stale claims across 22 files (3-agent verified)` (`6e9fb2a`, 2026-08-02)
- **Scope:** §"Building blocks (committed order)" — the "Next critical path" line and items 5–10

**⚠️ SUPERSEDED 2026-08-01.** This "Next critical path" list is a mid-2026-07 planning snapshot and no
longer reflects reality. Items **5 (host app), 9 (metadata) and 10 (wireless CarPlay) are DONE and
hardware-validated** — the app renders 4K video, plays all-rates audio, sends touch/HID over `CH_INPUT`,
surfaces NowPlaying/Nav metadata, and runs wireless CarPlay end to end. Current state lives in
`CLAUDE.md`, docs/carplay/05_METADATA_AND_CONTROLS.md (wireless + metadata) and docs/ops/02_TESTING.md (test plan). Item **8 (move SETUP app-side)
is now DONE for wired** (2026-08-08; toggle default flipped **ON** 2026-08-09 — see step 8 + docs/carplay/02_SESSION_LIFECYCLE.md).
(The wireless flip of app-driven SETUP landed 2026-08-10.)
**CORRECTED 2026-08-10: item 6 (nav voice / mic) is ALSO DONE** — see step 6 for the code paths and
the owner's hardware confirmation. Every numbered item 1-10 is now complete. What remains is NOT on
this list: it is the protocol surface we declare-but-do-not-serve plus workstreams C/D — Enhanced
Siri (AuxIn/AuxOut), MainBuffered audio, the HID knob/telephony descriptors, `lane_guidance` and
param 30, `displayPanels[]`, and two DataStream stubs. Tracked in the session task list and
`../ops/04_OPEN_ITEMS.md` §4 (the 08-10 handoff it originally cited was superseded).

## docs/carplay/07_PHONE_SIDE.md — Phone-side CarPlay bring-up (box ⇄ iPhone)

### R-07-1 · "This setup has no NCM" no longer holds repo-wide — an opt-in MFi-over-NCM bridge exists

- **Verdict:** CORRECTED
- **Landed:** `Docs QC: batch-fix the remaining audit findings across 51 files` (`a08b0eb`, 2026-08-16)
- **Scope:** §"iap2d — the accessory handshake (this box's job)"

**UPDATED 2026-08-16 — "this setup has no NCM" no longer holds repo-wide.** It still holds for THIS
wired CCPA path as shipped: `iap2d` drives the local chip and speaks no NCM at all. But the repo now
carries its own MFi-over-NCM bridge for the Raspberry-Pi / NCM bring-up boxes, which have neither a
local chip nor `CH_MFI`: `ccpa/mfid` (box-side service, default bind `0.0.0.0:7789`), `crates/mfi-wire`
(the `MFI1` framing), `host/mfi-probe` (the client), the remote backend in
`crates/vendor/wireless/src/mfi_local.rs`, and a client hook in `airplayd` itself
(`ccpa/airplayd/src/main.rs`, `mfi_wire::client::{cert,sign}`). All of it is opt-in behind
`CARPLAY_MFI_ADDR` (e.g. `192.168.50.2:7789`) — unset, which is the CCPA's own case, leaves the local
i2c path byte-for-byte unchanged. It is a bring-up instrument, not a production path, and no key
material crosses it: the genuine coprocessor signs, the socket only relays request/response frames.

### R-07-2 · The Status section's "Remaining: the real host app" clause is stale

- **Verdict:** SUPERSEDED
- **Landed:** `Docs QC: batch-fix the remaining audit findings across 51 files` (`a08b0eb`, 2026-08-16)
- **Scope:** §"Status" — the "Remaining" clause

**SUPERSEDED 2026-08-16 — the "Remaining" clause above is stale; retained for history.** The macOS
host app is DONE and hardware-validated: `host/CarPlayHost/carlink_macOS` (VideoToolbox decode,
audio, touch/media-key uplink, mic, Settings/YAML config push over OCBM) — README.md §Architecture,
docs/carplay/00_ARCHITECTURE.md §"The host app owns the EVOLVING session". What remains is tracked in the current
`docs/SESSION_HANDOFF_*.md`, not here.

## docs/carplay/02_SESSION_LIFECYCLE.md — Session Lifecycle & Host-Presence Management

### R-08-2 · Host-replacement detection must test `present` AND `subscribed`

- **Verdict:** CORRECTED
- **Landed:** uncommitted at time of writing (2026-08-27 lifecycle/health work)
- **Scope:** §"the state-machine ACTOR" replacement-host paragraph; `CT_HELLO` handler in `ocbmd`

**The bug.** The guard read `self.present && host_instance != inst`. But `CT_STOP` deliberately holds
`present` for `STOP_GRACE` (5 s) and drops only `subscribed` — so a normal app relaunch inside that
window is a **new pid with a new instance nonce against a still-present box**. Presence alone
classifies that as a replacement, sets `host_replaced`, and the next `CT_SUBSCRIBE` therefore forces
`rearm_presence_silently()`. The resulting `/tmp/host_present` dip is read by the supervisor as
GONE→PRESENT and answered with `wireless_down`/`wireless_up`.

Net effect: kill the app during a live wireless CarPlay session, relaunch within 5 s, and instead of
the warm reuse this document describes you get a radio cycle and a dropped session — the exact
scenario `STOP_GRACE` exists to protect.

> **SUPERSEDED 2026-09-03 (the bug above is still fixed; its *premise* is gone).** `STOP_GRACE` was
> removed: a clean `CT_STOP` is a session-end indicator and now takes the same immediate `go_idle`
> teardown heartbeat loss takes, dropping `present` **and** `subscribed` together. There is no
> within-grace window left to misclassify. The `present && subscribed` guard stays correct, for the
> reason it always should have carried: the flag means "the previous host died *mid-session*", and
> only a SUBSCRIBEd host has a session to die in.

**Why it had never been seen.** The GM head-unit host sent a **zero** instance nonce until 2026-08-27.
The box reads 0 as "not supplied", so replacement detection had never fired for that host at all.
Enabling the nonce is what made the path reachable — and first-reachability is exactly when a wrong
guard bites. Worth generalising: turning on a dormant code path is a behaviour change, not a
no-op, and deserves the review a new one would get.

**Also corrected:** `host_replaced` is now cleared in `go_idle()`. It is latched at `CT_HELLO` and
consumed only by the `CT_SUBSCRIBE` handler, and a host may legitimately send `CT_HELLO` without ever
subscribing (a link-only reattach that deliberately withholds the radio-wake edge), so a stale `true`
could otherwise survive to force an unwanted re-arm on a much later, unrelated `CT_SUBSCRIBE`.

### R-08-1 · Backpressure-not-drop reverses both "bounded queue with drop-oldest" and this document's own "drop the frame on `EAGAIN`"

- **Verdict:** REVERSED
- **Landed:** `Docs QC: fix 10 false claims found by auditing every .md against the code` (`1c591a0`, 2026-08-16)
- **Scope:** §"Governing principle: CarPlay is a live-state UI stream (think VNC)"; prerequisite item 3

This **supersedes** both the earlier "bounded queue with drop-oldest" idea *and* this document's own
original "drop the whole frame on `EAGAIN`" mechanism (2026-07-09, item 3 below). The reversal is
deliberate and recorded in docs/carplay/06_AV_PIPELINE.md (task #33 — DONE + hardware-validated at 4K@60, 2026-07-10) and in
docs/carplay/01_OCBM_PROTOCOL.md's OCBM channel section. Bounded per-stream queues exist on purpose: holding one frame is the
minimum apparatus needed to *express* backpressure. The no-stale-frame invariant is stronger than
before, not weaker — the read gate caps a video lane at one in-flight frame, where the old shared FIFO
coupled three streams (audit 2026-07-12 H1).

## docs/carplay/02_SESSION_LIFECYCLE.md — Live Session Observations (Instrumentation Findings) (2026-07-09)

### R-09-1 · The "Prioritized gaps" list (gaps 1–5) is all implemented, and the iAP2 coverage line is stale

- **Verdict:** SUPERSEDED
- **Landed:** `docs: annotate the remaining stale sections found by the scoped sweeps` (`65486fe`, 2026-08-10)
- **Scope:** §"Prioritized gaps toward full CarPlay protocol support"; §"Protocol coverage" (the iAP2 bullet)

**⚠️ SUPERSEDED 2026-08-10 — GAPS 1-5 ARE ALL IMPLEMENTED. Retained for history.**
This section read as a live priority list and was entirely stale. Verified against code:
1. `/command` handling — `modesChanged` is parsed and drives MainScreen focus state
   (`events.rs:213-226`); `disableBluetooth` is received and logged with **no action taken,
   deliberately** (`events.rs:227-242`).
2. Touch/HID input uplink — `airplayd` HID ingest on `127.0.0.1:9110`, `ocbmd INPUT_INGEST_ADDR`,
   `receiver::uplink`. Box-side arrival hardware-confirmed.
3. Voice/nav/Siri audio — the :9003 voice sink HAS an OCBM channel (`CH_ALT_AUDIO`: the
   `(9003u16, p::CH_ALT_AUDIO)` entry of `ocbmd/src/main.rs`'s `av_listeners` table, `:2388`);
   routing by `audioType` in `session.rs::setup_phase2` (`:871-879`); per-AU rate/channel tagging in
   `forward.rs::tag_voice` (`:84`). (Anchors re-verified 2026-08-16 — the original `ocbmd:2194` and
   `session.rs:794` line numbers had rotted.)
4. Mic uplink — `airplayd MIC_INGEST_ADDR 127.0.0.1:9112`, host `MicCapture.swift`, a REAL
   libfdk-aac AAC-ELD encoder in `eld-codec` (not the stub this doc implies).
   **Owner-confirmed on hardware 2026-08-10: Siri speech and two-way phone calls both work.**
5. Resolution control — app-authored via the pushed `VehicleConfig`; 1920x720 survives only as the
   app-less fallback in `base_device_config()`.

Also stale above: "iAP2 (iap2d): minimal so far — identification only" — iap2d now runs the whole
generated metadata declaration + subscribe plane (`features.rs`).

### R-09-2 · The "iOS-side cache pin" conclusion is wrong — the box was hardcoding 800×480

- **Verdict:** REFUTED
- **Landed:** `Baseline before 2026-07-25 QC remediation batch` (`18ba44b`, 2026-07-25)
- **Scope:** §"Video resolution — pinned at 800×480 iOS-side (NOT a box config gap)", incl. the "Reset lever" recipe

**CORRECTION (2026-07-10, see docs/carplay/06_AV_PIPELINE.md):** this section's conclusion is WRONG. The 800×480 was NOT an
iOS-side cache pin — the box was literally advertising 800×480 via a hardcoded override in
`ccpa/airplayd/src/main.rs` (was lines 342-343), overriding the 1920×720 struct default this section
reasoned from. Changing it to 1920×720 was honored by the iPhone on the next connection with NO forget
(proven on hardware). The "forget" exercise below was chasing a wrong hypothesis. Kept for history;
superseded by docs/carplay/06_AV_PIPELINE.md (diagnosis) + docs/carplay/03_SDK_GROUND_TRUTH.md (CarPlay SDK ground truth).

## docs/carplay/02_SESSION_LIFECYCLE.md — Lifecycle / Session-Management Hardening Plan

### R-11-1 · P0 has shipped and P1 is all but done — this plan is not a work queue

- **Verdict:** SHIPPED
- **Landed:** `Docs QC: batch-fix the remaining audit findings across 51 files` (`a08b0eb`, 2026-08-16)
- **Scope:** whole document; specifically §1, §3, "Supporting layers", and the Phasing list

**STATUS UPDATE 2026-08-16 — P0 HAS SHIPPED AND P1 IS ALL BUT DONE; this reads as a live TODO list and is not one.**
In `tools/session_supervisor.sh` today: the RECORD-milestone health gate (`scan_milestones` latches
`pair-verify OK` → `RECORD done`, published via `write_healthy` to `/tmp/session_healthy`), STUCK
counters that deliberately survive `teardown()`, the L1/L2/L3 ladder (L1 and L2 both run
`/script/phone_reset.sh` = `tools/phone_reset.sh`), the persistent L3 reboot budget in
`/etc/ccpa_reboot_count`, idle-gated peer-store mutation (`apply_pending` + `/tmp/peer_pending`), the
`/tmp/carplay_state` verdict, the count-bounded transition ring `/tmp/lifecycle.ndjson`, and a health
check over `airplayd` + `rx-connect` + `iap2d`. Elsewhere: `::respawn:` entries for BOTH `ocbmd` and
the supervisor (`ccpa/rootfs/etc/inittab`), Apple's 3 s/3 s/3 TCP keepalive on the iPhone-facing
control socket (`airplayd/src/main.rs::arm_keepalive`), and the host-side delegate wiring +
A/V-progress watchdog (`OCBMSessionCoordinator`, task #29). **One deviation from §1 as written:** the
supervisor, not airplayd, writes `/tmp/session_healthy` — it latches airplayd's own `RECORD done` log
line. **Still open INSIDE P1 (do not read the phasing list as fully closed):** the host-side *bounded*
resubscribe with an atomic `OCBMAVDecrypt.reset()` — `OCBMAVDecrypt` has no `reset()`, and the
resubscribe is the unbounded heartbeat / `SEV_HOST_GONE` retry in `OCBMClient`; and the ~12 s
RECOVERING grace pinned to the phone's active keepalive budget — the supervisor's graces are
milestone-aware but generous (`ESTAB_CONNECT_GRACE=90`, `ESTAB_STREAM_GRACE=30`). **Still open (P2):**
`CT_SESSION_EVENT` is still the 2-byte `[CT_SESSION_EVENT][SEV_*]` and was never widened to
`[state][reason]`; there is no single reason-carrying finalize; there is no `ccpad` binary;
pair-**resume** and `updateDisplayPanels` appear nowhere in the tree. Read the sections below as the
original plan record plus that open list, not as queued work.

## docs/carplay/06_AV_PIPELINE.md — Video Resolution Diagnosis (800×480 → 1920×720)

### R-12-1 · The "later RESOLVED — `uplink.rs::set_display` is applied" note was wrong when written

- **Verdict:** REFUTED
- **Landed:** `Docs QC: batch-fix the remaining audit findings across 51 files` (`a08b0eb`, 2026-08-16)
- **Scope:** §"Ruled out (EVIDENCED)" — the last bullet

**CORRECTED 2026-08-16 — the "later RESOLVED" note (added 2026-08-09, commit `89b90a5`) was wrong
when written.** It read: "`uplink.rs::set_display` exists and is applied; airplayd propagates dims."
`set_display` exists (`crates/vendor/receiver/src/uplink.rs:86`) but has **no caller anywhere in the
repo** — and had none at `89b90a5` either. What actually tracks the resolution is airplayd's OWN
`DISPLAY_WH` static, set from the resolved `DeviceConfig` at the end of `load_device_config()` and
used to scale the `:9110` HID ingest (`ccpa/airplayd/src/main.rs` — by symbol, because that file is
churning; as of 2026-08-16 `:539`, `:825`, `:1162`). That is the live touch path and it is correct.
The receiver's own control-in handler — `uplink::read_control` → `handle_touch` (`:292`/`:354`,
scaling at `:362`), which airplayd starts on `MIC_INGEST_ADDR` `127.0.0.1:9112` (the `:9110` in
`uplink.rs:266`'s doc comment is itself stale) — still scales against the never-updated 1920×720
default, so a non-1920×720 config would desync touch on THAT path. Latent as wired today (the host
app's touch goes to the `:9110` seam); flagged, not fixed.

### R-12-2 · The YAML/VehicleConfig follow-up landed — 1920×720 is no longer a hardcode

- **Verdict:** SHIPPED
- **Landed:** `Docs QC: batch-fix the remaining audit findings across 51 files` (`a08b0eb`, 2026-08-16)
- **Scope:** §"Follow-ups" — both bullets

**SUPERSEDED 2026-08-16 — the YAML/VehicleConfig path LANDED; 1920×720 is no longer a hardcode.**
`airplayd::load_device_config()` reads the host-pushed `/tmp/carplay_cfg.yaml`
(`CARPLAY_CFG_FILE_DEFAULT`, overridable by `CARPLAY_CFG_FILE`) and overlays `VehicleConfig::from_yaml`
onto `base_device_config()` **per control connection**, exactly the reconnect-consumed model
§"Why no forget is needed" predicts; the 1920×720 inside `base_device_config()` survives only as the
app-less fallback, and the resolved dims are published to airplayd's `DISPLAY_WH` at the end of
`load_device_config()`. (Cited by symbol on purpose — `ccpa/airplayd/src/main.rs` is actively churning;
as of 2026-08-16 those are `:663`, `:514`/`:530`, `:577`, `:582-583`, `:825`.) Note the
`uplink::set_display(w,h)` half of the bullet below was NOT what shipped — see the correction in
§"Ruled out". Retained for history.

## docs/carplay/03_SDK_GROUND_TRUTH.md — CarPlay SDK Ground Truth (CarPlay SDK reference, derived from Apple's shipped CarPlaySimulator)

### R-13-1 · §10's mapping table had seven of ten rows stale in the under-reporting direction

- **Verdict:** CORRECTED
- **Landed:** `docs+code: corrected doc-vs-code sweep — the audit that missed docs/ops/04_OPEN_ITEMS.md, done properly` (`152cd0c`, 2026-08-10)
- **Scope:** §10 — "Mapping to ccpa_custom + prioritized gaps"

**⚠️ CORRECTED 2026-08-10 — this table had SEVEN of ten rows stale in the "we have not built this"
direction.** That is the failure mode that matters most here: under-reporting our own capability is
what produced a wrong status report to the owner (see docs/ops/04_OPEN_ITEMS.md item 6 and the process note in the
session task list). Every ✅ above is cited to code. Note the bias direction — a doc that
under-reports wastes effort and misdirects planning; a doc that OVER-reports hides outages. Both are
defects; audit for both.

### R-13-2 · The mic uplink's ✅ was true of the code path and FALSE of the outcome — Siri heard nothing

- **Verdict:** CORRECTED
- **Landed:** `eld-codec: stop emitting LD-SBR — this is why Siri never heard the mic` (`536dfb8`, 2026-08-16)
- **Scope:** §10's Siri / telephony / alt audio row

§10 marks the mic uplink ✅ with "a REAL AAC-ELD encoder (`eld-codec`, not a stub)" and
"**Owner-confirmed on hardware 2026-08-10**". The encoder is indeed real and the plumbing did
work end to end — but **iOS discarded every access unit**, so Siri heard nothing.

`AACENC_SBR_MODE` was left at fdk-aac's default of -1 (auto), and its `eldSbrAutoConfigTab`
turns LD-SBR **on** for mono 16 kHz below 28000 bps. The encoder was asked for 24000, so it
emitted AAC-ELD **v2**, whose ASC is the 7-byte `f8f0312c00bc00` rather than the plain 4-byte
`f8f03000`. iOS never negotiated SBR — it asks for `audioFormat 0x04000000`
(`kAirPlayAudioFormat_AAC_ELD_16KHz_Mono`) and builds its decoder from that constant alone.
**There is no ASC on the wire**, so nothing could report the mismatch: every AU was decrypted
correctly and then dropped. No error surfaced anywhere, which is why the hardware confirmation
looked clean.

Two things make this worth recording beyond the fix itself:

- **The bug was already caught and nobody saw it.** `eld-codec`'s own test
  `eld_16k_mono_asc_matches_iphone` (`crates/vendor/eld-codec/src/lib.rs`) asserts the 4-byte
  ASC and *was failing* — but it sits behind the `mic-uplink-eld` feature and needs fdk-aac
  present, so it never ran. A test that cannot run is not a test.
- **"Owner-confirmed on hardware" confirmed the wrong layer.** What was observed was that the
  uplink path carried data, not that Siri responded to it. This is the same shape as `R-49-7`,
  where A/V health was read as whole-session health: two planes fail independently, and the
  silent one gets credited by the loud one.

SBR is now pinned off explicitly (`AACENC_SBR_MODE = 0`) so no future bitrate change can
re-enable it through the auto table. Found and fixed by the Raspberry Pi / AAOS port session;
verified here against `crates/vendor/eld-codec/csrc/eld_shim.c` and the commit.


## docs/carplay/04_CAPABILITIES_AND_CONFIG.md — YAML / VehicleConfig Framework (task #5)

### R-14-1 · The host-side Swift symbols named throughout are stale

- **Verdict:** STALE
- **Landed:** `Docs QC: flag docs/carplay/04_CAPABILITIES_AND_CONFIG.md's dead host-side Swift symbols` (`14981e7`, 2026-08-16)
- **Scope:** whole document — every host-side symbol reference (§Model, §Files, §Controllability, the video-counter note)

**⚠️ STALE HOST-SIDE SYMBOLS (2026-08-16).** Several Swift symbols named below no longer exist; the
app was refactored around `VehicleConfigModel` and nothing swept this file.
`AppDelegate.vehicleConfigYAML(width:height:)` was deleted (the push is `VehicleConfigModel.shared`);
`DisplayResolution.saved` / `.defaultResolution` are gone (use
`VehicleConfigModel.persistedMainResolution()`); `changeResolution` / `customResolution` have **zero**
hits anywhere under `host/`. `reinitializeAdapterSession` DOES survive. Also landed since: the "never
transmitted" implicit video counter and its "durable hardening (separate task)" both shipped — the box
stamps a per-frame `seq` and the host resyncs from it, with no implicit increment left — and the
"4 unit tests" figure is now 34. Grep before relying on any host-side symbol below.

## docs/carplay/06_AV_PIPELINE.md — Touch / HID Input Uplink (task #20): report + plan

### R-15-1 · The touch/HID uplink plan has shipped — it is a plan record, not a work queue

- **Verdict:** SHIPPED
- **Landed:** `Docs QC: batch-fix the remaining audit findings across 51 files` (`a08b0eb`, 2026-08-16)
- **Scope:** whole document; specifically §2 (the `mic-uplink` claim), §4 Phase 1, §5 Phases 2–3, §6

**STATUS UPDATE 2026-08-16 — SHIPPED. This is a plan record, not a work queue.** Phase 1
(single-touch), Phase 3 (buttons/commands, task #35) and the Phase-4 microphone all landed; Phase 2
multi-touch landed box-side and is driven by the **Android** host (hardware-verified 2026-08-15).
`CH_INPUT` + `INPUT_TOUCH` live in `crates/ocbm-proto`, ocbmd relays to `127.0.0.1:9110`, airplayd owns
that listener and drives `hid::touch_report_normalized` → `events::send_hid_report`, and the macOS host
sends via `OCBMClient.sendTouch`. **The one real remaining gap:** the macOS host's two-finger delegate
`AppDelegate.carPlayView(_:didMultiTouchTwo:)` is an empty stub, so the box's two-finger descriptor is
exercised from `CarlinkAndroid` but never from macOS. §2's claim that airplayd builds without
`mic-uplink` was true when written and is FALSE today — see the correction there.

## docs/host/00_MACOS_HOST_APP.md — Host-app CarPlay-SDK adherence audit + plan (12-agent)

### R-17-1 · Most of the 12-agent audit plan has shipped; five items remain open

- **Verdict:** SHIPPED
- **Landed:** `Docs QC: batch-fix the remaining audit findings across 51 files` (`a08b0eb`, 2026-08-16)
- **Scope:** whole document — Tier 0 #1–#3, Tier 1 #4/#5/#6, Tier 2 #7–#10, Tier 3 #11–#13, Tier 4

**⚠️ STATUS SWEEP 2026-08-16 — most of this plan has SHIPPED; read the per-item notes below.**
Closed since the audit: Tier 0 #1–#3 (all three — the Tier-0 header originally read "BOTH RESOLVED" and
undercounted; it is corrected in place below); Tier 1 #4
(the command surface) and #6 (HELLO/HELLO_ACK); Tier 2 #7 (config completeness — `enablesUIAppearance`/
`enablesMapAppearance`, `viewAreas`/`safeArea`, `hidConfig`/`primaryInput` all emit, and the preset list
carries a single 4K entry) and #9 (closed by the "or relabel the status" branch, not by wiring a reset —
the primitives it named are absent from every tree in this repo's history); Tier 3 #11 (the six legacy types are deleted) and #12 (closed by
DELETING `SessionRecorder`/`ProtocolLogger` in 5756f36, 2026-08-01; live diagnostics are `StreamMetrics`/`StreamMetricsMonitor`
+ `FileLogger`); and the Tier 4 HEVC roadmap item (decode is implemented). Still open: Tier 1 #5
(multi-touch — `didMultiTouchTwo`/`didTouch` are still empty bodies), Tier 2 #8 (touch aspect from the
decoded frame), HALF of Tier 2 #10 (an idle timeout no longer counts toward the 5-error streak, but the
read loop still calls `ClearPipeStallBothEnds` on EVERY timeout and there is still no retry backoff),
and the rest of the Tier 4 roadmap. Tier 3 #13 was real when written but has since been closed by
removal — see the note there.

## ../carplay/05_METADATA_AND_CONTROLS.md (`../carplay/05_METADATA_AND_CONTROLS.md`) — Metadata catalog & HID control semantics

### R-20M-1 · The Q1 summary table's four "NO — iap2d is auth+identify only" verdicts are false; iap2d runs the generated declare/subscribe metadata plane

- **Verdict:** SUPERSEDED
- **Landed:** `feat: app-driven doctrine (docs/carplay/04_CAPABILITIES_AND_CONFIG.md) + workstreams A-E` (`f3fa61d`, 2026-08-10) — the commit that put `arm_metadata_policy` into `ccpa/iap2d/src/main.rs`; the annotation itself was written in `Docs QC: batch-fix the remaining audit findings across 51 files` (`a08b0eb`, 2026-08-16)
- **Scope:** "Q1 SUMMARY TABLE — category → transport → available on our stack today", and the "Why NowPlaying/nav/call are 'NO'" paragraph immediately below it

**⇒ SUPERSEDED 2026-08-16 — the four "NO, iap2d is auth+identify only" verdicts were true when written
and are now false; the rows above have been rewritten in place.** `iap2d` arms the app-pushed metadata
policy (`Iap2Config::load().arm_metadata_policy()`, once at startup and again at SendIdentify), and once
`State::Identified` is reached it walks `carplay_iap2_core::features::active(...)` and fires every
`Start*Updates` in the active tier. Params 6/7 of the `0x1D01` Identify and that subscribe list are
GENERATED FROM THE SAME TABLE (`crates/vendor/iap2-core/src/features.rs`, docs/carplay/05_METADATA_AND_CONTROLS.md), so the
declared-vs-subscribed mismatch this section described cannot recur by construction. Inbound updates go
through `metadata::dispatch(msg_id, body)` onto the `:9004` seam, and album artwork is reassembled by
`metadata::Artwork` off the session-2 File Transfer. `declare_wired` is still `false` (the
`message::build_ident_info_with` call site in `ccpa/iap2d/src/main.rs`'s `Action::SendIdentify` arm — the
old `main.rs:174,212` anchors are dead), but that flag only adds the wired-CarPlay ids `0x4301`/`0x4300`;
it never gated the metadata plane. docs/carplay/02_SESSION_LIFECYCLE.md:41-42 carries the same correction. Historical text follows.

### R-20M-2 · §2.7.4's conclusion is wrong — display features bit `0x10` is Touchpad, and `dPadSupport` contributes nothing to `displays[].features`

- **Verdict:** REFUTED
- **Landed:** `Baseline: commit the 2026-07-29/30 review + Simulator-verification work` (`f88989d`, 2026-07-31); the "SOURCING CORRECTED 2026-08-16" paragraph inside the block landed later, in `Docs QC: batch-fix the remaining audit findings across 51 files` (`a08b0eb`, 2026-08-16)
- **Scope:** §2.7.4 and everything derived from it below — including the indented "When D-pad support is on…" conclusion, §2.7.6's fix steps, and the first Caveats bullet (which carries its own inline REFUTED note)

**⚠️ CORRECTED 2026-07-30 — THIS SECTION'S CONCLUSION IS WRONG. READ THIS BOX FIRST.**

The actual bit assignment is not the one derived below. R14G17
`AppleCarPlay/Sources/AirPlayCommon.h:209-213` and the Simulator's
`DisplayFeatures.init(airPlayValue:)` (@0x100269210) / `DisplayFeatures.rawValue` (@0x1002692a0) agree
on every bit R14G17 defines:

| bit | meaning | source |
|-----|---------|--------|
| 0x02 | Knobs | R14G17 + Simulator |
| 0x04 | LowFidelityTouch | R14G17 + Simulator |
| 0x08 | HighFidelityTouch | R14G17 + Simulator |
| **0x10** | **Touchpad** | R14G17 + Simulator |
| **0x20** | **DirectionButtons** | **Simulator ONLY** |

**SOURCING CORRECTED 2026-08-16** — this box used to read "two independent normative sources agree" over
all five rows and to cite `AirPlayCommon.h:210-213` for the table. That header defines only FOUR values
(`kAirPlayDisplayFeatures_{Knobs,LowFidelityTouch,HighFidelityTouch,Touchpad}`), and `DirectionButtons`
has **zero hits across all 267 R14G17 files** — it postdates the 2017 drop, exactly the class of gap
CLAUDE.md warns about ("silence in R14G17 is not an answer"). The load-bearing conclusion is unaffected:
both sources independently put **Touchpad at 0x10**, which is all the refutation below needs.

Two specific errors in the derivation below:
1. **`[HIDConfig+0x24]` is `touchpadSupport`, not the D-pad bool.** Verified against the full
   `CarPlayConfigs.HIDConfig` ivar layout (+0x18 `knobSupport` … +0x3c `dPadSupport`,
   +0x3d `notificationButton`). `dPadSupport` sits at +0x3c.
2. **Enum case 3 is `Touchpad`, not `Direction Buttons`.** Case 4 is Direction Buttons, and in
   `HIDConfig.displayFeatures` it is appended from **`steeringWheelSupport`** (+0x39).

**`dPadSupport` contributes NOTHING to `displays[].features`.** It only gates the D-Pad
`hidDevices[]` entry. So the claim "D-pad reports are only routed by iOS if the target display
advertises `features & 0x10`" is unfounded — in Apple's own D-pad templates that bit comes from
`touchpadSupport`, and bit 0x20 is never set by any shipped template.

The value we emit (`0x1A` under `CARPLAY_DPAD`) is unchanged and remains hardware-validated,
and is defensible by coincidence — Apple's `Standard.yaml` also yields 0x1A. But
what it *means* is Knobs|HighFidelityTouch|**Touchpad**, i.e. we advertise a touchpad we do not back
with a device. Correcting it is a wire change requiring a hardware session. See the corrected table
in `crates/vendor/receiver/src/info.rs` (`build_info`'s `disp_features` comment).

Everything below is retained for provenance. Do not build on it.

---

## docs/20W — Wireless CarPlay feasibility & implementation research (`../wireless/00_WIRELESS_CARPLAY.md`)

### R-20W-5 · §1.4's bring-up scripts are the IW416 MAPPING, not the live path — and reading them as live cost a session

- **Verdict:** CORRECTED (supersession note added in place)
- **Landed:** 2026-08-28
- **Scope:** §1.4 "Bring-up scripts"; `ccpa/rootfs/script/radio_hal.sh`; `ccpa/rootfs/script/attach_bluetooth.sh`

**The failure.** A GM head-unit session had a healthy OCBM link, a proven MFi relay and a successful
`CT_SUBSCRIBE`, and no Bluetooth whatsoever — no `hci0`, no pairing, nothing on the iPhone. The only
visible symptom was `hciattach` reporting `Can't set line discipline: Invalid argument`, sitting
directly beneath a flawless firmware download (`ChipID 7201`, 160,900 bytes, `Download Complete`).

**Root cause.** `/script/radio_hal.sh` and `/script/radio_detect.sh` were NOT INSTALLED on the unit.
The supervisor invokes the seam as `sh /script/radio_hal.sh bt_on >/tmp/bt.log 2>&1`, inside a
detached `setsid sh -c` whose **exit status it never reads** — so a missing file produced no error
anywhere. `hci_uart` is a loadable module on this box; without the seam nothing extracts it from
`/lib/firmware/nxp/iw416_ko.tar.gz`, so the `n_hci` line discipline is never registered and
`hciattach` cannot create `hci0`. Every layer above reported success.

`ocbm_install.sh --full` ships the seam and §"The file set" already warns that a box getting
`session_supervisor.sh` without it "has no radio bring-up at all ... a total failure, not a degraded
one". This unit had the supervisor and not the seam — the exact state that comment predicts, reached
by targeted pushes (`ocbm_push.sh` defaults to `ocbmd` + `carplay-wireless` and nothing else) rather
than an install.

**Why the doc mattered.** §1.4 presents `bt_on.sh -> attach_bluetooth.sh -> insmod hci_uart.ko` as
the bring-up sequence. It was read as current, and `attach_bluetooth.sh` was patched first. That
patch is correct in isolation and was verified working — and changed nothing, because the supervisor
does not call it. A stale "how it works" section is not harmless background; it is an active
misdirection at exactly the moment someone is debugging.

**What changed.**
- §1.4 now carries a supersession note pointing at docs/wireless/01_BT_AND_RADIO.md and this entry.
- `radio_hal.sh` gained a descriptor-independent recovery: if the descriptor names no line
  discipline it finds `*hci_uart*.ko` in the tarball itself. A recovery path that depends on the
  thing that failed (`radio_detect.sh` writing `/tmp/radio_caps`) is not a recovery path.
- `radio_hal.sh` now logs `FATAL` naming both `RADIO_BT_LDISC_KO` and the tarball when `n_hci` is
  still unregistered, so the fault is reported where it happens instead of three steps downstream.
- `attach_bluetooth.sh` self-extracts the module too, for the legacy path.

**General lessons worth keeping.**
1. `/tmp` is a tmpfs and the box is USB-powered, so *unplugging it to move it between bench and head
   unit is a power loss*. Both `/tmp/radio_caps` and the extracted `.ko`s vanish on every move. Any
   bring-up step that caches into `/tmp` must be able to rebuild from persistent storage, every boot.
2. A call whose exit status is unread will eventually be a call to something that is not there.
3. When behaviour and a document disagree, the document is a hypothesis. Confirm who actually calls
   a script before fixing it.


### R-20W-1 · This research is largely BUILT — implementation status as of 2026-07-14

- **Verdict:** SHIPPED
- **Landed:** `Baseline before 2026-07-25 QC remediation batch` (`18ba44b`, 2026-07-25) — the earliest commit in current history that carries this banner; the banner is dated 2026-07-14 and so predates the squash
- **Scope:** whole document — specifically §3 (the design that was built) and §4 (the gap analysis, since closed)

**IMPLEMENTATION STATUS — updated 2026-07-14.** This began as research and is now largely BUILT.
**Device-verified:** Phase 0 (radios + kernel gate), A1 (BT pair/SDP), A2 (iAP2/MFi/identify), and
**A3 handoff** — the iPhone prompts for CarPlay, sends `0x5702`, we answer `0x5703`, and it joins the
`wlan0` AP + gets DHCP (`192.168.43.100`). **Code-complete, not yet run to video:** A3 A/V —
`rx-connect` (mDNS `_airplay._tcp` + connect-out on `wlan0`) + `airplayd` (RTSP `:5000`),
auto-orchestrated from `carplay-wireless` after `0x5703`. The §3 design is what was built; the §4 gap
analysis is largely closed. The §7 `carlink_linux` C reference drove the implementation, and a
12-agent code study (2026-07-13) pinned the identify fix that unlocked the handoff. **For the current
state, deployed-vs-pending binaries, remaining work, gotchas, and the exact test procedure, see
the most recent `docs/SESSION_HANDOFF_*.md` (there is no root `HANDOFF.md` — corrected 2026-08-10).**

### R-20W-2 · §1.6's "`session_supervisor.sh` has zero wireless references" is a 2026-07-13 snapshot

- **Verdict:** STALE
- **Landed:** `Docs QC: batch-fix the remaining audit findings across 51 files` (`a08b0eb`, 2026-08-16)
- **Scope:** §1.6 "Current operating mode", the `session_supervisor.sh` bullet

**STALE 2026-08-16 — 2026-07-13 snapshot only, superseded in §4 below.** `tools/session_supervisor.sh`
is now ~61 KB (62,685 B) and mentions `wireless` on ~180 of its lines, including
`wireless_owns_session()` and the wired-preempt path. Left unchanged as the historical record.

### R-20W-3 · §3.2's "idle = both armed" is pre-doctrine — arming begins only after the app's config push

- **Verdict:** CORRECTED — doctrine
- **Landed:** `feat: app-driven doctrine (docs/carplay/04_CAPABILITIES_AND_CONFIG.md) + workstreams A-E` (`f3fa61d`, 2026-08-10)
- **Scope:** §3.2 "Two always-listening ingress agents"

**Re-scoped per docs/carplay/04_CAPABILITIES_AND_CONFIG.md (directive 3) — the §3.2 text above is pre-doctrine:** "idle = both armed"
begins only *after* the host app has connected and pushed config. The box holds IDLE, radios
un-armed, until the config push; from that point on both agents arm concurrently as described.

### R-20W-4 · §4's seven-item gap analysis is CLOSED — all seven shipped

- **Verdict:** SUPERSEDED
- **Landed:** `docs: annotate the remaining stale sections found by the scoped sweeps` (`65486fe`, 2026-08-10); the path qualifications and the re-measured `session_supervisor.sh` figures came in `Docs QC: batch-fix the remaining audit findings across 51 files` (`a08b0eb`, 2026-08-16)
- **Scope:** §4 "Gap analysis — what must be ADDED"; also the §1.3/§1.6 "no hostapd/wpa_supplicant, zero wireless references" observation and the "parked mic issue" lead

**⚠️ CLOSED 2026-07-17 (docs/wireless/00_WIRELESS_CARPLAY.md), annotated 2026-08-10, paths qualified 2026-08-16,
re-qualified 2026-09-03 (bt-common split). All seven items
shipped.** Retained as the original scoping. Everything named below is in `crates/vendor/wireless/src/`
unless stated otherwise: 1 radio bring-up -> `bt_bringup.rs`; 2 wireless iAP2 messages -> `bt_driver.rs`
+ `crates/vendor/iap2-core/src/message.rs`; 3 credential handoff -> `wifi_handoff.rs`; 4 iAP2-over-BT ->
`crates/bt-common/src/{rfcomm,sdp_server,ssp_agent}.rs` (moved out of `vendor/wireless` 2026-09-03); 5 discovery -> the `rx-connect` crate (`RX_IFACE=wlan0`);
6 airplayd on the WiFi link -> `av.rs`; 7 supervisor orchestration -> the dual-transport
`tools/session_supervisor.sh` + `reconnect.rs`.

Also superseded in this document: the "no hostapd/wpa_supplicant, zero wireless references in
session_supervisor.sh" observation describes the 2026-07-13 snapshot ONLY — `tools/session_supervisor.sh`
is now ~61 KB (62,685 B / 996 lines) and mentions `wireless` on ~180 of them (208 occurrences,
re-measured 2026-08-16; the "~181" recorded on 2026-08-10 matched the file as it stood that day, which
carried 182), incl. `wireless_owns_session()` and a wired-preempt path. And the "parked mic issue" lead
is CLOSED: the mic uplink ships and is device-confirmed; the `audioFormats` hypothesis was not the cause.

---

## docs/carplay/04_CAPABILITIES_AND_CONFIG.md — VehicleConfig / AccessoryConfig / hidConfig field glossary

### R-22-1 · The glossary's inert-field list is only as good as its last edit — eight of the fields it names are armed today

- **Verdict:** CORRECTED
- **Landed:** `Docs QC: batch-fix the remaining audit findings across 51 files` (`a08b0eb`, 2026-08-16) — the "EXTENDED 2026-08-16" half; the block itself was introduced by `docs: accuracy audit — fix ~30 wrong/stale claims across 22 files (3-agent verified)` (`6e9fb2a`, 2026-08-02)
- **Scope:** the whole glossary — this is the preamble that governs how every field entry below it is read

**⚠️ Wired vs inert (added 2026-08-01).** This glossary describes the *full Apple schema* — a field
appearing here is a capability the config UI can express, **not proof the box acts on it**. Many fields
are parsed/persisted but **not yet emitted or honoured on the wire**. The host app keeps its own list,
`SettingsWindow.inertKeys`, which appends a "⚠️ Not yet implemented on the box" marker to each field it
names. As of 2026-08-01 that set was:
`name`, `enablesVideoPlayback`, `enablesMainBufferedAudio`, `enablesEnhancedSiri`, `primaryInput`,
`mediaButtonsSupport`, `telephonyButtonsSupport`, `knobSupport`, `knobSupportsHomeAndBackButton`,
`knobSupportsNudge`, `touchpadSupport`, `touchpadButtonsSupport`, `touchScreenHighFidelity`,
`touchScreenSupportsCancel`, `enablesUIAppearance`, `enablesMapAppearance`, `enablesCornerMasks`,
`enablesFocusTransfer`, `enablesUIContext`, `enablesUISync`, `enablesFileTransfer`, `enablesLogTransfer`,
`enablesVehicleDataProtocol`, `enablesDCX`.

**⚠️ corrected 2026-08-10, EXTENDED 2026-08-16 — that set is only as good as its last edit.** On
2026-08-10 it still had `enablesCornerMasks`, `knobSupport` and `telephonyButtonsSupport` marked inert
although airplayd ARMS all three; fixed the same day. **Five more entries listed above are ALSO armed
today and must not be read as inert:** `enablesMainBufferedAudio`, `enablesLogTransfer`,
`enablesUIAppearance`, `enablesMapAppearance`, `enablesFocusTransfer`. All eight are armed per config
push from `ccpa/airplayd/src/main.rs`'s config-apply block, via `levers::set_mainbuffered`,
`levers::set_cornermasks`, `levers::set_logtransfer`, `levers::set_ui_appearance`,
`levers::set_map_appearance`, `levers::set_focus_transfer`, `events::set_knob_advertised` and
`events::set_telephony_advertised`. *(The old `main.rs:624/626/641` line anchors are dead — name the
setter, not the line.)* GROUND TRUTH is `vehicle_config.rs`'s accessors plus the `levers::`/`events::`
calls in `ccpa/airplayd/src/main.rs` — not this list, and not the app's.

(This is the generic `inertMarker` set; a few other dead fields — e.g. `rightHandDrive`, `nightMode` —
carry their own inline "not implemented" note in their entry below instead of appearing in this list.)
Treat any capability below as a *declaration of intent* unless its wiring is confirmed in
`info.rs`/`vehicle_config.rs`. (`dPadSupport` is NOT inert — it gates the `hidDevices[]` entry — but per
../carplay/05_METADATA_AND_CONTROLS.md §2.7.4 it contributes nothing to `displays[].features`.)

---

## docs/carplay/06_AV_PIPELINE.md — Alt / cluster video

### R-23-1 · `send_request_ui_url` / `send_stop_ui_url` no longer exist

- **Verdict:** STALE
- **Landed:** `fix: apply 16 audit findings (security gate, races, robustness, dead code)` (`f989a17`, 2026-08-08) — the dead-code sweep that deleted both; recorded in the document by `Docs QC: batch-fix the remaining audit findings across 51 files` (`a08b0eb`, 2026-08-16)
- **Scope:** §3 Mechanism 1, the `requestUI` bullet

**UPDATED 2026-08-16 — the URL-addressed pair this line named is GONE.** When it was written (2026-08-01) `send_request_ui_url`/`send_stop_ui_url` did exist; commit `f989a17` (2026-08-08, dead-code sweep) deleted both. The only cluster-addressed emitters today are `send_show_ui(stream_id, url)` (events.rs:791) and `send_stop_ui(stream_id)` (events.rs:814), both keyed by `params["uuid"]`.

### R-23-2 · §4's "what the box must ADD" is built and validated — the type-111 cluster path ships

- **Verdict:** SHIPPED
- **Landed:** `docs: reconcile documentation with current code state (6-agent audit)` (`89b90a5`, 2026-08-09)
- **Scope:** §4 "(c) What the box (`receiver_core`) must ADD to negotiate + forward a 2nd screen"

**STATUS (implemented + validated — supersedes the "must ADD" framing of this section):** the type-111
cluster path is BUILT. `crates/vendor/receiver/src/session.rs` has a live `111 =>` SETUP arm that binds
a second screen socket and `spawn_screen(… 9005 …)`; `info.rs` emits the second `displays[]` cluster
entry with `altScreenURLs` (the altScreen guard is lifted). The box forward of type-111 is **gated
behind `events::nav_forward()`** (default-OFF — a measured-failure guard; the gate stays
app-commandable, and per docs/carplay/04_CAPABILITIES_AND_CONFIG.md its *default* belongs in the pushed config, the in-binary default
being interim) so the cluster can't starve the main 4K stream when nav is off. The host decrypts it on a dedicated per-lane queue + decoder. The cluster's later ~1–2 fps
starvation was root-caused + fixed 2026-08-09 (host per-lane decrypt decouple — see
the 2026-08-09 handoff, since retired — current pick-up is `../ops/04_OPEN_ITEMS.md`). The `ncm_carplayd` line numbers in §4 below are historical.

---

## docs/carplay/03_SDK_GROUND_TRUTH.md — CarPlay Simulator conformance sweep, 2026-07-12

### R-26-1 · The `setNightMode` half of the truthfulness issue shipped; `VehicleConfig.nightMode` and `rightHandDrive` are still inert

- **Verdict:** PARTIALLY SUPERSEDED
- **Landed:** `Docs QC: batch-fix the remaining audit findings across 51 files` (`a08b0eb`, 2026-08-16)
- **Scope:** "Truthfulness issue in host UI"

**SUPERSEDED IN PART 2026-08-16.** The `setNightMode` half was implemented exactly as prescribed:
`events::send_set_night_mode` emits `{type:"setNightMode", params:{nightMode:<bool>}}`, driven over OCBM
by `CMD_NIGHT_MODE` (`ocbm-proto` 0x10) from the host Controls window, and the help text was corrected on
2026-08-01 (see the `nightMode` entry in docs/carplay/04_CAPABILITIES_AND_CONFIG.md). What remains true: **`VehicleConfig.nightMode` itself
is still not parsed and still not an `/info` key** — the live path is a runtime appearance command, not
that config field — and **`rightHandDrive` is still fully inert** (no `/info` key, no parser). docs/carplay/04_CAPABILITIES_AND_CONFIG.md's
entries now say so instead of claiming either is sent.

### R-26-2 · Four of the "missing optional features" have since landed

- **Verdict:** PARTIALLY SUPERSEDED
- **Landed:** `Docs QC: batch-fix the remaining audit findings across 51 files` (`a08b0eb`, 2026-08-16)
- **Scope:** "Missing optional features the Simulator exposes (not defects)"

**PARTIALLY SUPERSEDED 2026-08-16 — four of these have since landed.**
(1) **Telephony control** ships as HID **uid 5** (Apple's exact `HIDTelephonyCreateDescriptor`), gated on
the app-pushed `hidConfig.telephonyButtonsSupport`; call *state* is declared **and** subscribed at the
default *proven* metadata tier (`features.rs` `call_state`, 0x4154/0x4155).
(2) **Rotary knob** ships as HID **uid 4** under `hidConfig.knobSupport`. Steering wheel and touchpad are
still absent.
(3) **`enablesMapAppearance` / `mapAppearance`** is wired end to end: `info.rs` `add_appearance_keys`
emits `uiAppearanceMode/Setting` + `mapAppearanceMode/Setting` on every screen dict (without them iOS
silently drops the runtime commands), `levers::set_ui_appearance` / `set_map_appearance` arm from the
pushed config, and `events::send_ui_appearance_update` / `send_map_appearance_update` are host-driven.
(4) The alt display dict now carries **`showsInstruments: true` + `initialURL:
maps:/car/instrumentcluster/map`**, so iOS registers the cluster panel; `altDisplayPanels[]` is parsed by
`vehicle_config.rs` but is still not emitted as a separate panel array.
Cluster Show-flags, lane guidance and the full maneuver list remain open.

---

## docs/carplay/06_AV_PIPELINE.md — Audio formats capability

### R-27-1 · docs/carplay/04_CAPABILITIES_AND_CONFIG.md's `enablesMainBufferedAudio` "better quality than realtime" is wrong — buffered improves delivery, not fidelity

- **Verdict:** CORRECTED
- **Landed:** `docs/carplay/04_CAPABILITIES_AND_CONFIG.md: CarPlay capability research + roadmap; doc corrections` (`631cec6`, 2026-08-02)
- **Scope:** corrects `../carplay/04_CAPABILITIES_AND_CONFIG.md`'s `enablesMainBufferedAudio` entry; raised from the "`mainBuffered` — where Apple's audio effort actually went" section of docs/carplay/06_AV_PIPELINE.md. Nothing in docs/carplay/06_AV_PIPELINE.md itself is corrected by it. **Note:** docs/carplay/04_CAPABILITIES_AND_CONFIG.md's entry has since been corrected in place (it now carries "CORRECTED 2026-08-02: this previously said 'better quality than realtime'"), so the moved text's present tense reads as of 2026-08-02, not today.

**Correction to a claim elsewhere in this repo.** `../carplay/04_CAPABILITIES_AND_CONFIG.md`
(`enablesMainBufferedAudio`) describes buffered as *"better quality than realtime."* **It does not
change the codec** — the same `audioFormat` bitmask applies, and there is no higher-fidelity entry to
select. What improves is *delivery integrity*: no jitter dropouts, no packet-loss concealment
artifacts, real ASRC for clock skew. Audibly better in practice; **better delivery of the same stream,
not higher fidelity.** WWDC frames it as responsiveness and dropout survival, never as quality.

---

## docs/wireless/00_WIRELESS_CARPLAY.md — Wireless metadata, iAP2-over-AirPlay tunnel fix attempt, 2026-07-22

### R-30-1 · The `CARPLAY_WIRELESS_METADATA` env gate is interim scaffolding, not the design

- **Verdict:** CORRECTED — doctrine
- **Landed:** `feat: app-driven doctrine (docs/carplay/04_CAPABILITIES_AND_CONFIG.md) + workstreams A-E` (`f3fa61d`, 2026-08-10)
- **Scope:** whole document — every place it treats the env var as the feature's sole gate, including the supervisor launch line and the plan to promote it to always-on

**CORRECTED 2026-08-10 — app-driven doctrine (docs/carplay/04_CAPABILITIES_AND_CONFIG.md).** This doc treats the
`CARPLAY_WIRELESS_METADATA` env var as the feature's sole gate — including hardcoding it into the
wireless supervisor launch line and the plan to promote it to always-on there. Per docs/carplay/04_CAPABILITIES_AND_CONFIG.md such
on-box env levers are interim scaffolding; the gate migrates to app-pushed config, and box-side
promotion-to-default is no longer the path. The historical record below is unchanged.

### R-30-2 · Hygiene item 3's "FF5A/link framing is not a live open question" steered the investigation away from what Apple requires

- **Verdict:** PARTIALLY SUPERSEDED
- **Landed:** `Correct the corrections: fix errors found in docs/ops/03_REFERENCE_INDEX.md and docs/carplay/03_SDK_GROUND_TRUTH.md` (`5b01c6a`, 2026-07-25); the superseding document itself is `docs/carplay/03_SDK_GROUND_TRUTH.md: correct docs/wireless/00_WIRELESS_CARPLAY.md against the licensed R14G17 SDK source` (`aefe6f0`, 2026-07-25)
- **Scope:** the hygiene-corrections block (item 3), and the `AirPlayReceiverSession.c:5486` citation in "Root cause chain (grounded this session)"

**PARTIALLY SUPERSEDED — see `../carplay/03_SDK_GROUND_TRUTH.md`.** This doc's own
hygiene item 3 tells readers the FF5A/link-framing question is "very likely dead code in practice…
not a live open question" — that steered the investigation away from what Apple's Integration Guide
(line 289) actually requires: a full iAP2 handshake, including link framing, on this channel. See
docs/carplay/03_SDK_GROUND_TRUTH.md §5. Also note the citation `AirPlayReceiverSession.c:5486` below does **not** resolve in the
licensed R14G17 copy — `AirPlayReceiverSessionSendiAPMessage` is at **`:5332`** (docs/carplay/03_SDK_GROUND_TRUTH.md, and
`docs/ops/03_REFERENCE_INDEX.md` for where that source lives).
docs/carplay/03_SDK_GROUND_TRUTH.md is derived by reading that source directly; where the two disagree, docs/carplay/03_SDK_GROUND_TRUTH.md is correct.

### R-30-3 · The transport premise is superseded — wireless iAP2 rides an RCS DataStream (SETUP stream type 130)

- **Verdict:** SUPERSEDED — transport premise only
- **Landed:** `Baseline: commit the 2026-07-29/30 review + Simulator-verification work` (`f88989d`, 2026-07-31) — the commit that added `../carplay/05_METADATA_AND_CONTROLS.md`
- **Scope:** the whole document's transport premise; its message-shape and link-layer content is unaffected

Transport premise superseded by `../carplay/05_METADATA_AND_CONTROLS.md`: wireless
iAP2 rides a RemoteControlSession DataStream (SETUP stream type 130), not `iAPSendMessage`
inside `POST /command`. Message-shape and link-layer content here remains valid.

### R-30-4 · Four hygiene corrections to this document's original claims (2026-07-24, docs/carplay/05_METADATA_AND_CONTROLS.md/36)

- **Verdict:** CORRECTED
- **Landed:** `Baseline before 2026-07-25 QC remediation batch` (`18ba44b`, 2026-07-25) — the earliest commit in current history carrying the block; it is dated 2026-07-24 and predates the squash
- **Scope:** the `Data` key casing, the "off by default" claim, the FF5A-link-wrapper framing hedge, and outcome 2's "second `0x1D01 Identify`" next-experiment

**Hygiene corrections (2026-07-24, docs/carplay/05_METADATA_AND_CONTROLS.md/36 — do not re-derive from this doc's original claims
below, kept for historical record only):**
1. **`data` key casing**: the code below is described as sending capital `Data`. The CURRENT code
   (`events.rs::send_iap_message`) sends lowercase `data` — a later fix, device-confirmed correct
   (capital `Data` got a uniform `RTSP/1.0 400 Bad Request`). The code is right; this doc's prose was
   never updated. Don't "fix" the code back to match this page.
2. **"Off by default" (line ~38)**: no longer true. `crates/vendor/wireless/src/av.rs::ensure_av_layer`
   hardcodes `CARPLAY_WIRELESS_METADATA=1` unconditionally for every wireless session — the wired
   supervisor launch line is what stays unset (by design, see docs/wireless/00_WIRELESS_CARPLAY.md #2.1/#1.1).
3. **FF5A-link-wrapper framing (outcome 3's "framing" possibility, and `dispatch_iap_tunnel_message`'s
   FF5A-strip branch)**: docs/carplay/05_METADATA_AND_CONTROLS.md §4-7 traced the ENTIRE chain from `iAPSendMessage` through
   `AirPlaySender`/`CoreAccessories`/`accessoryd`'s XPC boundary via direct disassembly and found NO
   framing/wrapper is added or expected anywhere — bare `msg_payload` is exactly what's expected. The
   FF5A hedge is very likely dead code in practice (confirmed in docs/carplay/05_METADATA_AND_CONTROLS.md #2.7), not a live open
   question.
4. **The "second `0x1D01 Identify`" next-experiment (outcome 2, below)**: docs/carplay/05_METADATA_AND_CONTROLS.md §6-7 found
   `accessoryd`'s real registration gate is NOT an Identify-completion boolean flag at all — it's a
   connection/endpoint registration keyed by a per-feature capability from SETUP negotiation. The
   actual next experiment implemented is `sessionManagementInfo` (docs/wireless/00_WIRELESS_CARPLAY.md #2.1), not a second Identify
   message. See docs/wireless/00_WIRELESS_CARPLAY.md for the current, verified implementation plan and its Phase 5 (isolated,
   incremental wireless-Identify message-id changes) for what remains of the ORIGINAL outcome-2 idea.

### R-30-5 · "Deployed state"'s claim that the box is CURRENTLY armed is long stale, and that launch line no longer exists

- **Verdict:** STALE
- **Landed:** `Docs QC: batch-fix the remaining audit findings across 51 files` (`a08b0eb`, 2026-08-16)
- **Scope:** "Deployed state (this session)" — the `session_supervisor.sh` ARM-line bullet only; the rest of that section is a historical deploy record

**Hygiene correction 5 (added 2026-08-16), for the "Deployed state" section below:** the claim that
**the box is CURRENTLY armed** is long stale, and the launch line it describes no longer exists.
`CARPLAY_WIRELESS_METADATA` is set ONLY at the wireless spawn site — unconditionally, per item 2 —
in `crates/vendor/wireless/src/av.rs`; the wired ARM line in `tools/session_supervisor.sh` carries
`OCBM_FWD_ENC=1` plus the optional cornermask/logTransfer/mainBuffered flags and never this one,
which that file states in a comment beside the ARM block. Everything else in that section is a
historical deploy record and stands as written.

---

## docs/carplay/02_SESSION_LIFECYCLE.md — Session management: Apple + GM/CINEMO vs `ccpa_custom`, 2026-07-23

### R-31-1 · Session-management configuration is app-authored and pushed, not box-owned

- **Verdict:** CORRECTED — doctrine
- **Landed:** `feat: app-driven doctrine (docs/carplay/04_CAPABILITIES_AND_CONFIG.md) + workstreams A-E` (`f3fa61d`, 2026-08-10)
- **Scope:** the document's direction statements — the "Scope discipline" paragraph's priority direction, the box-declared `stopSessionReasons` set (§6), and the box-side ownership-tracking proposal (Summary items 1–2)

**CORRECTED 2026-08-10 — app-driven doctrine (docs/carplay/04_CAPABILITIES_AND_CONFIG.md).** This doc's direction statements are
superseded: "the ccpa/box side is the actual lever for session behavior; host-app downstream", the
box-declared `stopSessionReasons` set, and the proposal for box-side ownership tracking. Per
docs/carplay/04_CAPABILITIES_AND_CONFIG.md, session-management configuration content (including declared reason sets) is app-authored
and pushed at initialization, with the box as relay. The historical record below is unchanged.

### R-31-2 · The transport premise is superseded — wireless iAP2 rides an RCS DataStream (SETUP stream type 130)

- **Verdict:** SUPERSEDED — transport premise only
- **Landed:** `Baseline: commit the 2026-07-29/30 review + Simulator-verification work` (`f88989d`, 2026-07-31) — the commit that added `../carplay/05_METADATA_AND_CONTROLS.md`
- **Scope:** the document's wireless-metadata transport premise (§7 in particular); message-shape and link-layer content is unaffected

Transport premise superseded by `../carplay/05_METADATA_AND_CONTROLS.md`: wireless
iAP2 rides a RemoteControlSession DataStream (SETUP stream type 130), not `iAPSendMessage`
inside `POST /command`. Message-shape and link-layer content here remains valid.

### R-31-3 · The `ccpa_custom` comparand column is a 2026-07-23 snapshot — the real grace constants differ, and live in `ocbmd`

- **Verdict:** STALE
- **Landed:** `Docs QC: fix 10 false claims found by auditing every .md against the code` (`1c591a0`, 2026-08-16)
- **Scope:** §3's `ccpa_custom` column (the timeout/grace row) and its takeaway, plus Summary item 5

**STALE COMPARAND 2026-08-16 — the `ccpa_custom` column is a 2026-07-23 snapshot and is left
unchanged as the historical record.** Current values: the presence graces live in
**`ccpa/ocbmd/src/main.rs`, not `session_supervisor.sh`** — that script defines no heartbeat
constant at all and is purely edge-triggered on `/tmp/host_present`. `HEARTBEAT_GRACE` was widened
**3 s → 10 s** on 2026-07-25 (audit QC #428, `const HEARTBEAT_GRACE`) and `REARM_HOLD = 2 s`
(`const REARM_HOLD`) was added later. `STOP_GRACE = 5 s` (the clean-`CT_STOP` hold) also existed here
and was **removed 2026-09-03** — `CT_STOP` now tears down immediately; see R-08-2's superseding note.
Cite the constant names, not line numbers, which have already drifted once (`:561/:566/:579` →
`:562/:567/:580`).
The `~5 s` RECOVERING grace quoted below never corresponded to a constant — it was a notional design value.
`ESTAB_CONNECT_GRACE=90s` / `ESTAB_STREAM_GRACE=30s` (`tools/session_supervisor.sh`, the `# --- tunables`
block, `:36-37`) are still accurate. **The comparison's conclusion strengthens rather than weakens:** against the real 10 s,
this project's grace is ~5× GM's `SESSION_FINALIZE_DELAY=2000ms`, not "more than double" as §below
derives from the 2026-07-23 figure.

## docs/carplay/05_METADATA_AND_CONTROLS.md — Wireless CarPlay Metadata: GM/CINEMO Real-World Reference, 2026-07-23

### R-32-1 · docs/carplay/03_SDK_GROUND_TRUTH.md partially supersedes §4-7 — the "no framing needed anywhere" reading is over-read

- **Verdict:** PARTIALLY SUPERSEDED
- **Landed:** `docs/carplay/03_SDK_GROUND_TRUTH.md: correct docs/wireless/00_WIRELESS_CARPLAY.md against the licensed R14G17 SDK source` (`aefe6f0`, 2026-07-25)
- **Scope:** §4–§7; the whole-document banner
- **Migration note:** Split from a single banner: the strikethrough half of it was itself reversed on 2026-08-16 — that clause is `R-32-2`. The two closing sentences moved here are the docs/carplay/03_SDK_GROUND_TRUTH.md authority note that followed the banner.

**PARTIALLY SUPERSEDED — see `../carplay/03_SDK_GROUND_TRUTH.md`.** §4-7's "no framing needed anywhere" is over-read (docs/carplay/03_SDK_GROUND_TRUTH.md §5), and ~~the `iAPChannel` SETUP-gate framing has no basis in the SDK (docs/carplay/03_SDK_GROUND_TRUTH.md §3)~~
docs/carplay/03_SDK_GROUND_TRUTH.md is derived by reading Apple's licensed R14G17 accessory SDK source directly; the conclusions
corrected there were derived by inference. Where the two disagree, docs/carplay/03_SDK_GROUND_TRUTH.md is correct.

### R-32-2 · The `iAPChannel` SETUP-gate framing half is REVERSED — the gate is CONFIRMED

- **Verdict:** REVERSED
- **Landed:** `Docs QC: half-retract docs/carplay/03_SDK_GROUND_TRUTH.md §3, and drop a confounded device claim` (`cfa6447`, 2026-08-16)
- **Scope:** §4–§7; reverses half of `R-32-1`
- **Migration note:** Reverses the struck-through half of `R-32-1`. `R-32-1`'s other half (the "no framing needed anywhere" over-read) still stands.

— **that half is REVERSED 2026-08-16: the SETUP-gate framing is CONFIRMED** by `CarPlaySDK.framework` 509.11 and iOS 27 `AirPlaySender`; it is absent from R14G17 only because it postdates 2017 (docs/carplay/03_SDK_GROUND_TRUTH.md §3 correction).

### R-32-3 · Transport premise superseded by docs/carplay/05_METADATA_AND_CONTROLS.md — §5–§7's conclusions about WHERE the bug is are dead

- **Verdict:** PARTIALLY SUPERSEDED
- **Landed:** `Baseline: commit the 2026-07-29/30 review + Simulator-verification work` (`f88989d`, 2026-07-31)
- **Scope:** §4 (reversal carried inline), §5, §6, §7

Transport premise superseded by `../carplay/05_METADATA_AND_CONTROLS.md`: wireless
iAP2 rides a RemoteControlSession DataStream (SETUP stream type 130), not `iAPSendMessage`
inside `POST /command`. GM findings (§1–§3) stand and message-shape/link-layer content remains
valid. **§4 carries the reversal inline; §5–§7 do not — their conclusions about WHERE the bug is,
and §7's closing "instrument the `iAPSendMessage` path" step, are dead: the inbound channel was
never answered at all.** `POST /command` is NOT removed — it remains the working *outbound*
carrier (docs/carplay/05_METADATA_AND_CONTROLS.md §1.1).

### R-32-4 · §2's "no second socket" conclusion is REFUTED — a dedicated DataStream socket does exist

- **Verdict:** REFUTED
- **Landed:** `Docs QC: fix regressions the correction pass itself introduced` (`5cc08dc`, 2026-08-16)
- **Scope:** §2 — "What GM DOES do — a real, separate iAP2 socket, not an AirPlay tunnel"

**CORRECTED 2026-08-16 — the "no second socket" conclusion is REFUTED.** Apple's real wireless iAP2 carrier is a RemoteControlSession DataStream (SETUP stream **type 130**), and its request carries `wantsDedicatedSocket = true`, which per `docs/carplay/05_METADATA_AND_CONTROLS.md` §1.3 *"is mandatory for Apple's receiver, which fails setup with `-6714` otherwise"*; the response must return a `dataPort`. Our own implementation binds that listener (`crates/vendor/receiver/src/session.rs`, the type-130 SETUP arm), and the shared-connection alternative is explicitly NOT IMPLEMENTED. So a dedicated socket **does** exist in Apple's path — what was right in the analysis below is that it is not a *separate GM-style iAP2-over-TCP link*, but a dedicated DataStream socket negotiated inside the same AirPlay session. See `docs/carplay/05_METADATA_AND_CONTROLS.md` §1.3.

### R-32-5 · The "mandatory / `-6714`" strength in `R-32-4` is NOT independently sourced

- **Verdict:** CORRECTED
- **Landed:** `Docs QC: four more regressions of mine, incl. an unsourced claim I amplified` (`0d59e33`, 2026-08-16)
- **Scope:** §2 — qualifies `R-32-4`
- **Migration note:** Added seven minutes after `R-32-4` landed, because the "mandatory" strength had already been over-propagated into other documents. It qualifies `R-32-4`; it does not withdraw it.

*(**Sourcing caveat, added 2026-08-16:** the "mandatory / `-6714`" strength comes from docs/carplay/05_METADATA_AND_CONTROLS.md §1.3 and is NOT independently sourced — `-6714` appears nowhere in this repo outside docs/carplay/05_METADATA_AND_CONTROLS.md and the notes citing it, and `receiver/src/session.rs`'s type-130 arm records that Apple's receiver "does not branch on this at all", consuming the key only for a log line. What IS solid: every capture on record sends `wantsDedicatedSocket = true`, and we bind the `dataPort` listener. Treat the necessity as unproven.)*

### R-32-6 · §4's "dead code at the call-site level" retraction is itself REVERSED — the first pass was right

- **Verdict:** REVERSED
- **Landed:** `Baseline: commit the 2026-07-29/30 review + Simulator-verification work` (`f88989d`, 2026-07-31)
- **Scope:** §4 — "Resolved (2026-07-23): Apple does NOT use a second socket"
- **Migration note:** Three layers deep: a 2026-07-23 first pass named the mechanism; a 2026-07-23 second pass retracted it as "dead code"; 2026-07-25 hardware reversed the retraction. The retracted original text is still in the document under the marker, prefaced by its own "do not act on the 'dead code' claim" note. NOTE FOR THE MERGE PASS: the moved text opens with a literal `# ` H1 — that is how the block was written, and it is left as written rather than downgraded, per the move-verbatim rule. It will render as a heading inside the ledger unless someone deliberately demotes it.

**RETRACTION REVERSED — 2026-07-25. THE FIRST PASS WAS RIGHT.**

The paragraph below declared `carEndpoint_createiAPChannelIfNeeded` and
`carEndpoint_sendCommandOverRCSChannel` "dead code at the call-site level". **That retraction was
wrong, and it is the most expensive single error in this repo.** Those two functions ARE the
mechanism: modern iOS creates a RemoteControlSession DataStream (SETUP stream type 130) for iAP and
carries the entire iAP2 link inside it. Confirmed 2026-07-25 from both ends — the phone opens the
stream, we answer it, and iAP2 frames flow. See `docs/carplay/05_METADATA_AND_CONTROLS.md`.

**What went wrong:** a static caller search (`axt`) found no direct callers, and the absence was
recorded as "dead code" rather than as "no callers found *by this method*". The call is made
indirectly, through the endpoint-activation path. **Absence of an observed caller is not evidence of
absence of a caller.** A correct mechanism, already identified on 2026-07-23, was argued away — and
the project then spent two days searching elsewhere.

The `iAPSendMessage`/`APAccTransportClientEndpointForwardData` path described below is real; it is
the **outbound** carrier, and it still works on iOS 27. What was wrong is the conclusion that the RCS
channel was incidental to it. It is the other way round: the RCS channel is the inbound path, and
without it nothing comes back.

### R-32-7 · §7's closing "instrument the `iAPSendMessage` path" step is dead

- **Verdict:** SUPERSEDED
- **Landed:** `Docs QC: fix 10 false claims found by auditing every .md against the code` (`1c591a0`, 2026-08-16)
- **Scope:** §7's closing paragraph; also the origin of the same advice in docs/carplay/05_METADATA_AND_CONTROLS.md §III.6 and Part IV item 1

**⚠️ DO NOT DO THIS AS WRITTEN (docs/carplay/05_METADATA_AND_CONTROLS.md, 2026-07-25).** This paragraph is the origin of the same
stale advice carried into docs/carplay/05_METADATA_AND_CONTROLS.md §III.6 and Part IV item 1. Instrumenting the `iAPSendMessage`
path today captures only the pre-RCS DETECT/SYN — `events.rs::send_iap_message:516-538` prefers the
DataStream sink once the channel exists — and there is no wired `enabledFeatures` to compare
against (wired iAP2 is `iap2d` over `/dev/android_iap2`, with no AirPlay SETUP). The blocker was
the unanswered stream-130 SETUP. Use the phone's own `accessoryd` iAP2 packet trace over USB
(docs/carplay/05_METADATA_AND_CONTROLS.md §6) instead; it is a strict superset of what this paragraph proposes.

### R-32-8 · The nine decompilation dumps behind §5–§7 are GONE

- **Verdict:** STALE
- **Landed:** `Docs QC: batch-fix the remaining audit findings across 51 files` (`a08b0eb`, 2026-08-16)
- **Scope:** §Artifacts — and, through it, the evidence base for §5–§7

**GONE, verified 2026-08-16.** The nine decompilation dumps below were written to per-session
scratchpad space (`/private/tmp/claude-501/-Users-zeno/<session-uuid>/scratchpad/`), which is not
preserved; the session directory no longer exists. **They are the entire primary evidence base for
§5-§7** — the `accessoryd` `processIncomingData` chain, the two silent-drop dictionary lookups, and the
hard-coded `type=5`/`transportType=4` literals rest on them and cannot now be re-read. The conclusions
below stand as written, but treat them as a secondary record: re-derive before relying on a detail, by
disassembling afresh from `~/Downloads/ios27_extract_24A5390f/` per docs/ops/03_REFERENCE_INDEX.md §D — `split/AirPlaySender`
and `split/CoreAccessories` are there, but **`accessoryd` itself is NOT in the split cache** (only
`mined/accessoryd.strings.txt`, plus `split/AccessoryDaemonSupport` / `split/CarAccessoryDaemon`).
Note also that §7's closing "instrument the `iAPSendMessage` path" step is separately dead per the
docs/carplay/05_METADATA_AND_CONTROLS.md banner above.


## docs/carplay/05_METADATA_AND_CONTROLS.md — Combined Report: Session Management & Wireless Metadata, 2026-07-23

### R-33-1 · The header's and Part IV's "box-side first, host-app downstream" frame is inverted

- **Verdict:** CORRECTED
- **Landed:** `feat: app-driven doctrine (docs/carplay/04_CAPABILITIES_AND_CONFIG.md) + workstreams A-E` (`f3fa61d`, 2026-08-10)
- **Scope:** Header ¶2, Part IV

**CORRECTED 2026-08-10 — app-driven doctrine (docs/carplay/04_CAPABILITIES_AND_CONFIG.md).** This report inherits docs/carplay/02_SESSION_LIFECYCLE.md's direction
statements — the header's "box-side handling is the actual lever … host-app changes are downstream"
and Part IV's "ccpa/box-side first, host-app downstream" planning frame. Per docs/carplay/04_CAPABILITIES_AND_CONFIG.md that priority
is inverted: configuration and session-behavior content is app-authored and pushed, with the box as
relay. The historical record below is unchanged.

### R-33-2 · docs/carplay/03_SDK_GROUND_TRUTH.md partially supersedes §III.4 — the `iAPChannel` SETUP-feature claim and the "no framing" conclusion

- **Verdict:** PARTIALLY SUPERSEDED
- **Landed:** `docs/carplay/03_SDK_GROUND_TRUTH.md: correct docs/wireless/00_WIRELESS_CARPLAY.md against the licensed R14G17 SDK source` (`aefe6f0`, 2026-07-25)
- **Scope:** §III.4
- **Migration note:** Split from a single banner. The 2026-08-16 clause that sat between these two sentences reversed the struck-through half and is `R-33-3`; the two sentences here are the docs/carplay/03_SDK_GROUND_TRUTH.md-era text either side of it, in their original wording.

**PARTIALLY SUPERSEDED — see `../carplay/03_SDK_GROUND_TRUTH.md`.** ~~§III.4's claim that `iAPChannel` is a negotiated SETUP feature key is unsupported — the string does not exist in R14G17 (docs/carplay/03_SDK_GROUND_TRUTH.md §3).~~ The "no framing" conclusion is over-read (docs/carplay/03_SDK_GROUND_TRUTH.md §5).
docs/carplay/03_SDK_GROUND_TRUTH.md is derived by reading Apple's licensed R14G17 accessory SDK source directly; the conclusions
corrected there were derived by inference. Where the two disagree, docs/carplay/03_SDK_GROUND_TRUTH.md is correct.

### R-33-3 · §III.4's `iAPChannel` claim is REVERSED back to CONFIRMED

- **Verdict:** REVERSED
- **Landed:** `Docs QC: half-retract docs/carplay/03_SDK_GROUND_TRUTH.md §3, and drop a confounded device claim` (`cfa6447`, 2026-08-16)
- **Scope:** §III.4; reverses half of `R-33-2`
- **Migration note:** Reverses the struck-through half of `R-33-2`. The "no framing" over-read in `R-33-2` still stands.

**REVERSED 2026-08-16: §III.4 is CONFIRMED.** The string is absent from R14G17 only because the gate postdates 2017; `CarPlaySDK.framework` 509.11 and iOS 27's `carEndpoint_createSetupRequestFeatureList` both carry `iAPChannel` (docs/carplay/03_SDK_GROUND_TRUTH.md §3 correction).

### R-33-4 · Transport premise superseded by docs/carplay/05_METADATA_AND_CONTROLS.md — Part I ¶2 is backwards and four sections' "next step" advice is dead

- **Verdict:** PARTIALLY SUPERSEDED
- **Landed:** `Baseline: commit the 2026-07-29/30 review + Simulator-verification work` (`f88989d`, 2026-07-31)
- **Scope:** Part I ¶2, §III.4's closing paragraph, §III.5's inference, §III.6, Part IV item 1

Transport premise superseded by `../carplay/05_METADATA_AND_CONTROLS.md`: wireless
iAP2 rides a RemoteControlSession DataStream (SETUP stream type 130), not `iAPSendMessage`
inside `POST /command`. Message-shape and link-layer content here remains valid. **Scope: §III.4's
closing paragraph, §III.5's inference, §III.6 and Part IV item 1 are premised on the refuted
carrier, and their "next step" advice is dead — see the inline notes there. Part I ¶2's "the gap is
the wire format inside that existing channel, not a missing transport" is backwards: the channel
never existed.** Note `POST /command` is NOT removed — it remains the working *outbound* carrier
(docs/carplay/05_METADATA_AND_CONTROLS.md §1.1) and the only one until the phone opens the channel.

### R-33-5 · §III.6's "instrument the path" next step is dead, and §III.5's registration hypothesis is dead on capture

- **Verdict:** SUPERSEDED
- **Landed:** `Docs QC: fix 10 false claims found by auditing every .md against the code` (`1c591a0`, 2026-08-16)
- **Scope:** §III.6, and §III.5's inference (its observations stand)
- **Migration note:** The capture anchor inside this text was itself corrected on 2026-08-16 (`:53-56`, was `:55-58`) in `Docs QC: batch-fix the remaining audit findings across 51 files` (`a08b0eb`); that correction is carried inline in the moved text rather than as its own entry, being an anchor fix rather than a change of claim.

**⚠️ DO NOT DO THIS AS WRITTEN (docs/carplay/05_METADATA_AND_CONTROLS.md, 2026-07-25). The impulse was right, the target wrong.**
(a) `iAPSendMessage` is no longer the carrier: `events.rs::send_iap_message` (`:516-538`) prefers
`datastream::send` and falls back to `POST /command` only before the RCS channel exists, so
instrumenting it captures one DETECT and one SYN and nothing else — already recorded in docs/carplay/05_METADATA_AND_CONTROLS.md
§1.1. (b) There is no wired `enabledFeatures` to diff against: wired iAP2 is `iap2d` over
`/dev/android_iap2` and performs no AirPlay SETUP. (c) The blocker was the stream-130 SETUP this
project never answered, not an ACC registration gate — and §III.5's registration hypothesis is dead
on capture: `docs/ops/captures/2026-07-25_iphone_iap2_trace_sess3.txt:53-56` (anchor corrected 2026-08-16,
was `:55-58`) shows `AirPlay; Event;
ATTACH; iAP2 accessory attached!` against a registered `<connectionUUID>_<endpointUUID>` pair with
our SYN reaching `iAP2PacketParseBuffer`. (§III.5's *observations* of `accessoryd` stand; only the
"silently dropped for want of registration" inference falls. Which carrier delivered those bytes is
a separate question `receiver/src/datastream.rs:256-260` still records as unresolved.)
**What to do instead:** the phone's own `accessoryd` iAP2 packet trace over USB (docs/carplay/05_METADATA_AND_CONTROLS.md §6). The
arm that genuinely needs hardware hours is the stream-130 accept path, key probe and RCS reassembly
— docs/carplay/05_METADATA_AND_CONTROLS.md §8 records ZERO hardware hours on it. Beware: a run with `CARPLAY_WIRELESS_METADATA`
unset reproduces the 07-31→08-10 outage signature (`session.rs:1000`) and will look like
confirmation of the theory above.

### R-33-6 · Part IV item 1's experiment is superseded — the missing piece was the unanswered stream-130 SETUP

- **Verdict:** SUPERSEDED
- **Landed:** `Docs QC: fix 10 false claims found by auditing every .md against the code` (`1c591a0`, 2026-08-16)
- **Scope:** Part IV item 1 (and item 3, which it confirms as already implemented)

**⚠️ SUPERSEDED (docs/carplay/05_METADATA_AND_CONTROLS.md).** Answered, and not by this experiment: the missing piece was the
unanswered stream-130 SETUP, not a capability gate on ACC registration. The wired-vs-wireless
`enabledFeatures` diff has no left-hand side (wired iAP2 does no AirPlay SETUP). See the note at
§III.6. **Item 3 below was the correct guess and is already implemented** — the tunnel runs its
own fresh iAP2 Identify (`crates/vendor/receiver/src/session.rs:1863-1869`,
`crates/vendor/receiver/src/iap_tunnel.rs`); it needs no further decompilation pass.


## docs/carplay/05_METADATA_AND_CONTROLS.md — Silverado (CINEMO r14) vs CT5 (CINEMO r17), 2026-07-23

### R-34-1 · Transport premise superseded by docs/carplay/05_METADATA_AND_CONTROLS.md — the GM/CINEMO findings themselves stand in full

- **Verdict:** PARTIALLY SUPERSEDED
- **Landed:** `Baseline: commit the 2026-07-29/30 review + Simulator-verification work` (`f88989d`, 2026-07-31)
- **Scope:** Whole document — the transport premise only

Transport premise superseded by `../carplay/05_METADATA_AND_CONTROLS.md`: wireless
iAP2 rides a RemoteControlSession DataStream (SETUP stream type 130), not `iAPSendMessage`
inside `POST /command`. Message-shape and link-layer content here remains valid. **The GM/CINEMO
findings below stand in full** — they describe GM's own implementation, not Apple's.

### R-34-2 · Where this document contrasts GM's dedicated socket with Apple "needing none", Apple does negotiate one

- **Verdict:** CORRECTED
- **Landed:** `Docs QC: fix regressions the correction pass itself introduced` (`5cc08dc`, 2026-08-16)
- **Scope:** §Bottom line's closing paragraph; qualifies `R-34-1`

Note only that
where this file contrasts GM's dedicated socket with Apple "needing none", Apple's type-130 SETUP
does carry `wantsDedicatedSocket = true` (mandatory; `-6714` otherwise, docs/carplay/05_METADATA_AND_CONTROLS.md §1.3) — a dedicated
DataStream socket negotiated inside the AirPlay session, not a separate GM-style iAP2 link.

### R-34-3 · The "mandatory / `-6714`" strength in `R-34-2` is NOT independently sourced

- **Verdict:** CORRECTED
- **Landed:** `Docs QC: four more regressions of mine, incl. an unsourced claim I amplified` (`0d59e33`, 2026-08-16)
- **Scope:** Qualifies `R-34-2`
- **Migration note:** Added seven minutes after `R-34-2`, because the "mandatory" strength had already been over-propagated. It qualifies `R-34-2`; it does not withdraw it. The identical caveat is `R-32-5` on docs/carplay/05_METADATA_AND_CONTROLS.md.

*(**Sourcing caveat, added 2026-08-16:** the "mandatory / `-6714`" strength comes from docs/carplay/05_METADATA_AND_CONTROLS.md §1.3 and is NOT independently sourced — `-6714` appears nowhere in this repo outside docs/carplay/05_METADATA_AND_CONTROLS.md and the notes citing it, and `receiver/src/session.rs`'s type-130 arm records that Apple's receiver "does not branch on this at all", consuming the key only for a log line. What IS solid: every capture on record sends `wantsDedicatedSocket = true`, and we bind the `dataPort` listener. Treat the necessity as unproven.)*


## docs/carplay/05_METADATA_AND_CONTROLS.md — Wireless Metadata: 12-Agent Code Audit + Stock Firmware Attempt, 2026-07-23

### R-35-1 · Box-side ownership of the `sessionManagementInfo` declaration is superseded by the app-driven doctrine

- **Verdict:** CORRECTED
- **Landed:** `feat: app-driven doctrine (docs/carplay/04_CAPABILITIES_AND_CONFIG.md) + workstreams A-E` (`f3fa61d`, 2026-08-10)
- **Scope:** Part 2 and Part 3's `sessionManagementInfo` recommendation

**CORRECTED 2026-08-10 — app-driven doctrine (docs/carplay/04_CAPABILITIES_AND_CONFIG.md).** This audit frames `sessionManagementInfo`
as "a cheap, additive, low-risk lever" to be declared box-side. Per docs/carplay/04_CAPABILITIES_AND_CONFIG.md its content — like all
`/info`/SETUP declaration content — is app-authored and pushed at initialization, with the box as
relay; box-side declaration ownership is superseded. The historical record below is unchanged.

### R-35-2 · docs/carplay/03_SDK_GROUND_TRUTH.md partially supersedes §1.3's channel priority and §1.1's iAPChannel-gate causal chain

- **Verdict:** PARTIALLY SUPERSEDED
- **Landed:** `docs/carplay/03_SDK_GROUND_TRUTH.md: correct docs/wireless/00_WIRELESS_CARPLAY.md against the licensed R14G17 SDK source` (`aefe6f0`, 2026-07-25)
- **Scope:** §1.1, §1.3, Part 2
- **Migration note:** Split from a single banner. The 2026-08-16 clause that sat between these two sentences reversed part of the §1.1 correction and is `R-35-3`; the text here is the docs/carplay/03_SDK_GROUND_TRUTH.md-era wording either side of it.

**PARTIALLY SUPERSEDED — see `../carplay/03_SDK_GROUND_TRUTH.md`.** §1.3 inverts the inbound channel priority — control-connection delivery is PRIMARY, not a contingency (docs/carplay/03_SDK_GROUND_TRUTH.md §1). §1.1's iAPChannel-gate **causal chain** (missing gate → iOS 400s every `iAPSendMessage`) is unsupported (docs/carplay/03_SDK_GROUND_TRUTH.md §3; docs/wireless/00_WIRELESS_CARPLAY.md attributes those 400s to the capital-`Data` bug). §Part 2's doubt about `iAPChannelInfo` (the `/info` key, a different thing) was right and still is.
docs/carplay/03_SDK_GROUND_TRUTH.md is derived by reading Apple's licensed R14G17 accessory SDK source directly; the conclusions
corrected there were derived by inference. Where the two disagree, docs/carplay/03_SDK_GROUND_TRUTH.md is correct.

### R-35-3 · Only the CAUSAL CHAIN falls — the `"iAPChannel"` `enabledFeatures` SETUP echo is CONFIRMED load-bearing

- **Verdict:** REVERSED
- **Landed:** `Docs QC: batch-fix the remaining audit findings across 51 files` (`a08b0eb`, 2026-08-16)
- **Scope:** §1.1; reverses part of `R-35-2`
- **Migration note:** Reverses only the part of `R-35-2` that read as condemning the SETUP echo itself. `R-35-2`'s actual correction — that the causal chain (missing gate → iOS 400s every `iAPSendMessage`) is unsupported — still stands, as does its note that Part 2's doubt about the *`/info`* key `iAPChannelInfo` was right.

**Only the causal chain: the `"iAPChannel"` `enabledFeatures` SETUP echo named in that chain is CONFIRMED load-bearing (REVERSED 2026-08-16)** by `CarPlaySDK.framework` 509.11 and iOS 27 `AirPlaySender`; it is absent from R14G17 only because it postdates 2017 (docs/carplay/03_SDK_GROUND_TRUTH.md §3 correction), and it is what opens the stream-130 channel docs/carplay/05_METADATA_AND_CONTROLS.md proved is the real carrier — do not strip it.

### R-35-4 · Transport premise superseded by docs/carplay/05_METADATA_AND_CONTROLS.md

- **Verdict:** PARTIALLY SUPERSEDED
- **Landed:** `Baseline: commit the 2026-07-29/30 review + Simulator-verification work` (`f88989d`, 2026-07-31)
- **Scope:** Whole document — the transport premise only

Transport premise superseded by `../carplay/05_METADATA_AND_CONTROLS.md`: wireless
iAP2 rides a RemoteControlSession DataStream (SETUP stream type 130), not `iAPSendMessage`
inside `POST /command`. Message-shape and link-layer content here remains valid.

### R-35-5 · Part 1 is FIXED — every finding 1.1–1.4 shipped, and every line anchor in Part 1 is stale

- **Verdict:** SHIPPED
- **Landed:** `Docs QC: batch-fix the remaining audit findings across 51 files` (`a08b0eb`, 2026-08-16)
- **Scope:** Part 1 (1.1–1.4), and Part 2's "confirmed absent" `sessionManagementInfo` finding

**STATUS UPDATE 2026-08-16 — Part 1 is FIXED; the status line below is the 2026-07-23 original.**
Part 1's findings (1.1–1.4) shipped via docs/wireless/00_WIRELESS_CARPLAY.md Phase 1/2 + Phase 5.0 and are live in source today:
`ensure_av_layer` verifies the resident `airplayd`'s `/proc/<pid>/environ` and claims
`/tmp/carplay_transport` at spawn time (`crates/vendor/wireless/src/av.rs`); `wireless_down` removes
that flag value-scoped and the health-milestone scanner reads `/tmp/airplayd_wl.log` for wireless
sessions (`tools/session_supervisor.sh`); the wireless Identify declares param 30, closing 1.2
(docs/wireless/00_WIRELESS_CARPLAY.md Phase 5.0); and 1.3/1.4's inbound gaps closed too — control-connection frames now route
through `iap_tunnel::handle_inbound` / `dispatch_iap_tunnel_message`
(`crates/vendor/receiver/src/session.rs`), and the brittle `"192.168.43."` peer-address test is gone.
Separately, `sessionManagementInfo`/`stopSessionReasons` — "confirmed absent" in **Part 2** — are now
declared in `crates/vendor/receiver/src/info.rs` and echoed in `session.rs`. Part 2's protocol
question was answered by docs/carplay/05_METADATA_AND_CONTROLS.md, not by any of these. **Every line anchor in Part 1 (`av.rs:160`,
`session_supervisor.sh:167`, `:383`) is stale — use the symbol names above.**


## docs/wireless/00_WIRELESS_CARPLAY.md — Implementation Plan: Wireless Metadata Fixes + Session Management, 2026-07-24

### R-36-1 · The env-var gating and box-side stop-reason set are superseded by the app-driven doctrine

- **Verdict:** CORRECTED
- **Landed:** `feat: app-driven doctrine (docs/carplay/04_CAPABILITIES_AND_CONFIG.md) + workstreams A-E` (`f3fa61d`, 2026-08-10)
- **Scope:** Phase 2.1, Phase 4.1, and every `CARPLAY_SESSION_MGMT` / `CARPLAY_WIRELESS_METADATA` gate

**CORRECTED 2026-08-10 — app-driven doctrine (docs/carplay/04_CAPABILITIES_AND_CONFIG.md).** This plan gates its features on the
`CARPLAY_SESSION_MGMT` / `CARPLAY_WIRELESS_METADATA` env vars as the primary controls and fixes the
stop-reason set in box source. Per docs/carplay/04_CAPABILITIES_AND_CONFIG.md both are superseded: declaration content is app-pushed,
and on-box env levers are interim scaffolding (app-less testing only). The §5.1/5.2 device-reject
evidence pinning the BT-time Identify remains valid and safety-critical (see docs/carplay/05_METADATA_AND_CONTROLS.md §6.2, docs/carplay/04_CAPABILITIES_AND_CONFIG.md). The historical record below is unchanged.

### R-36-2 · docs/carplay/03_SDK_GROUND_TRUTH.md partially supersedes this plan

- **Verdict:** PARTIALLY SUPERSEDED
- **Landed:** `docs/carplay/03_SDK_GROUND_TRUTH.md: correct docs/wireless/00_WIRELESS_CARPLAY.md against the licensed R14G17 SDK source` (`aefe6f0`, 2026-07-25)
- **Scope:** Whole document
- **Migration note:** Split from a single banner: what this banner ORIGINALLY went on to claim about §2.5 was itself retracted, by docs/carplay/03_SDK_GROUND_TRUTH.md itself — that is `R-36-3`. The two closing sentences moved here are the docs/carplay/03_SDK_GROUND_TRUTH.md authority note that followed the banner.

**PARTIALLY SUPERSEDED — see `../carplay/03_SDK_GROUND_TRUTH.md`.**
docs/carplay/03_SDK_GROUND_TRUTH.md is derived by reading Apple's licensed R14G17 accessory SDK source directly; the conclusions
corrected there were derived by inference. Where the two disagree, docs/carplay/03_SDK_GROUND_TRUTH.md is correct.

### R-36-3 · This banner's own §2.5 claim was retracted by docs/carplay/03_SDK_GROUND_TRUTH.md — the two channels had been conflated

- **Verdict:** CORRECTED
- **Landed:** `Docs QC: fix regressions the correction pass itself introduced` (`5cc08dc`, 2026-08-16)
- **Scope:** §2.5; corrects `R-36-2`'s original wording
- **Migration note:** A correction of a correction: `R-36-2`'s banner asserted a thing docs/carplay/03_SDK_GROUND_TRUTH.md had already retracted. ../ops/06_CORRECTIONS_LEDGER.md cites this specific case as one of the reasons the in-document blocks became a drift surface of their own.

**CORRECTED 2026-08-16: this banner previously said "§2.5 is wrong AND misattributed to the SDK source". docs/carplay/03_SDK_GROUND_TRUTH.md itself retracted that** — see docs/carplay/03_SDK_GROUND_TRUTH.md's §2.5 note: *"Both of those were wrong — the source says exactly what §2.5 says."* §2.5 concerns the **event** channel, where the reference writes no reply (still true: `crates/vendor/receiver/src/events.rs`, the event-channel reader emits none); docs/carplay/03_SDK_GROUND_TRUTH.md §2's "200 + empty-dict plist" finding concerns the **control** `POST /command` channel. The two were conflated. What docs/carplay/03_SDK_GROUND_TRUTH.md does supersede here is listed below.

### R-36-4 · Transport premise superseded by docs/carplay/05_METADATA_AND_CONTROLS.md

- **Verdict:** PARTIALLY SUPERSEDED
- **Landed:** `Baseline: commit the 2026-07-29/30 review + Simulator-verification work` (`f88989d`, 2026-07-31)
- **Scope:** Whole document — the transport premise only

Transport premise superseded by `../carplay/05_METADATA_AND_CONTROLS.md`: wireless
iAP2 rides a RemoteControlSession DataStream (SETUP stream type 130), not `iAPSendMessage`
inside `POST /command`. Message-shape and link-layer content here remains valid.

### R-36-5 · The Phase-5 "confirmed closed dead end as a class" conclusion is scoped and partly refuted

- **Verdict:** PARTIALLY SUPERSEDED
- **Landed:** `Docs QC: batch-fix the remaining audit findings across 51 files` (`a08b0eb`, 2026-08-16)
- **Scope:** §Results — the Phase 5.1/5.2/5.3 conclusion; also the same conclusion restated in §Phase 5
- **Migration note:** Two independent defects in one conclusion: (a) the scope (BT-time Identify only, not the tunnel arm) and (b) the mechanism (the reject payload is a generic marker, so what iOS objected to was never established). The two device reverts themselves are untouched — see `R-38-4`, the same correction applied to docs/wireless/00_WIRELESS_CARPLAY.md's copy of the conclusion.

**SCOPED AND PARTLY REFUTED 2026-08-16 — "as a class" is wrong twice over.** (a) **Scope:** every
reject above, and Phase 5.2's "not content-specific" reading, was measured on the **Bluetooth-time**
Identify. It says nothing about the Identify sent on the tunnel: the `TransportComponent::AirPlayTunnel`
arm in `crates/vendor/iap2-core/src/message.rs` declares the full metadata id set and iOS **accepted**
it on hardware 2026-07-25 (`RX 0x1D02 -> Identified`, 8 subscribes, docs/carplay/05_METADATA_AND_CONTROLS.md §6.6). That file's own
AirPlayTunnel comment says so: *"the literal rule 'any params-6/7 growth is rejected' cannot be what is
going on"*. (b) **Mechanism:** the `[len=0x0004][pid=0x0006]` payload this conclusion was read off is a
GENERIC rejection marker — docs/carplay/05_METADATA_AND_CONTROLS.md §7 records it arriving unchanged in a run where param 6 was the
ACCEPTED baseline, so what iOS objected to here was never established (docs/carplay/05_METADATA_AND_CONTROLS.md §6.2). The BT-time
Identify stays byte-pinned on the strength of the two device reverts — a safety constraint, not a
proven rule about params 6/7 (docs/carplay/04_CAPABILITIES_AND_CONFIG.md). When an Identify is rejected, ask the phone
(`idevicesyslog -u <udid> -p accessoryd`); do not bisect.


## docs/wireless/00_WIRELESS_CARPLAY.md — Wireless BT-retry transport-flag bug, 2026-07-24

### R-37-1 · The fix is not "queued" — both halves shipped 2026-07-24 and are still live

- **Verdict:** SHIPPED
- **Landed:** `Docs QC: batch-fix the remaining audit findings across 51 files` (`a08b0eb`, 2026-08-16)
- **Scope:** §Fix and §Status

**UPDATE — SHIPPED (deployed 2026-07-24; recorded here 2026-08-16).** Both fixes went out the same day
and were confirmed holding across repeated live BT `0x5702` retries — docs/wireless/00_WIRELESS_CARPLAY.md §Final shipped
configuration. Still live in `crates/vendor/wireless/src/av.rs`: `wait_visible` polls 60 × 100 ms, and
the `AV_LAYER_UP` latch short-circuits `ensure_av_layer` (extended 2026-07-31 with a `pid_alive`
re-check, so a crashed `airplayd` no longer stays latched-up forever). The `pgrep` fix was also
generalised beyond the full path: `running()` now tries the full path **and** the basename, because the
two userlands this daemon runs on match different things.


## docs/wireless/00_WIRELESS_CARPLAY.md — Wireless Identify Metadata Experiment: Results, 2026-07-24

### R-38-1 · §Conclusion's "remaining path needs dynamic (qemu) analysis" was wrong when written

- **Verdict:** PARTIALLY SUPERSEDED
- **Landed:** `docs/carplay/03_SDK_GROUND_TRUTH.md: correct docs/wireless/00_WIRELESS_CARPLAY.md against the licensed R14G17 SDK source` (`aefe6f0`, 2026-07-25)
- **Scope:** §Conclusion and remaining path; the Phase-5 experimental results stand

**PARTIALLY SUPERSEDED — see `../carplay/03_SDK_GROUND_TRUTH.md`.** §Conclusion's "remaining path needs dynamic (qemu) analysis" was wrong when written — the answer was in the licensed SDK already on disk (docs/carplay/03_SDK_GROUND_TRUTH.md §6). The Phase-5 experimental results themselves stand.
docs/carplay/03_SDK_GROUND_TRUTH.md is derived by reading Apple's licensed R14G17 accessory SDK source directly; the conclusions
corrected there were derived by inference. Where the two disagree, docs/carplay/03_SDK_GROUND_TRUTH.md is correct.

### R-38-2 · Transport premise superseded by docs/carplay/05_METADATA_AND_CONTROLS.md

- **Verdict:** PARTIALLY SUPERSEDED
- **Landed:** `Baseline: commit the 2026-07-29/30 review + Simulator-verification work` (`f88989d`, 2026-07-31)
- **Scope:** Whole document — the transport premise only

Transport premise superseded by `../carplay/05_METADATA_AND_CONTROLS.md`: wireless
iAP2 rides a RemoteControlSession DataStream (SETUP stream type 130), not `iAPSendMessage`
inside `POST /command`. Message-shape and link-layer content here remains valid.

### R-38-3 · §Summary's "the tunnel-side plumbing was already complete and correct" premise is false

- **Verdict:** REFUTED
- **Landed:** `Docs QC: batch-fix the remaining audit findings across 51 files` (`a08b0eb`, 2026-08-16)
- **Scope:** §Summary; scopes `R-38-2` for this document
- **Migration note:** Scopes `R-38-2`: the Phase-5 device evidence is unaffected by the transport correction because it never used that carrier — but the ABSENCE of metadata replies, which the experiments were reading as evidence about the Identify, is explained by the unanswered stream-130 SETUP instead.

**Scope for this document (added 2026-08-16).** The Phase-5 experiments below ran on the
**Bluetooth-time** Identify and never used that carrier, so their device evidence is unaffected.
What the transport correction does touch is §Summary's premise that "the tunnel-side
subscribe/dispatch plumbing was already complete and correct": it was not — the phone's stream-130
SETUP went unanswered, so the iAP channel never existed (docs/carplay/05_METADATA_AND_CONTROLS.md §1.3, §8) and the absent metadata
replies were never evidence about the Identify declaration. docs/carplay/05_METADATA_AND_CONTROLS.md §7 also records that the
params-6/7 conclusion here is scoped to the Bluetooth Identify only (see §Conclusion's SCOPE note).

### R-38-4 · The MECHANISM claim is corrected — NOT the outcome

- **Verdict:** CORRECTED
- **Landed:** `Docs QC: batch-fix the remaining audit findings across 51 files` (`a08b0eb`, 2026-08-16)
- **Scope:** §Conclusion and remaining path — "rejected regardless of which ids"
- **Migration note:** The distinction is the whole point of this entry: the two device reverts are untouched and the BT-time Identify stays byte-pinned because of them. What falls is the READING of why iOS rejected. The same correction was applied to docs/wireless/00_WIRELESS_CARPLAY.md's copy of the conclusion — `R-36-5`.

**CORRECTED 2026-08-16 — the MECHANISM claim, not the outcome.** "Rejected regardless of which ids"
was read off the `[len=0x0004][pid=0x0006]` payload decoded in §5.2. That payload is a **generic**
rejection marker: docs/carplay/05_METADATA_AND_CONTROLS.md §7 records it arriving unchanged in a run where param 6 was the ACCEPTED
baseline, so what iOS objected to here was never established (docs/carplay/05_METADATA_AND_CONTROLS.md §6.2). The two reverts stand as
device evidence and the BT-time Identify stays byte-pinned because of them — as a safety constraint,
not as a proven rule about params 6/7 (docs/carplay/04_CAPABILITIES_AND_CONFIG.md). Do not carry the generalisation to another
transport: the AirPlayTunnel Identify declares the full metadata id set and iOS accepted it on hardware
2026-07-25 (docs/carplay/05_METADATA_AND_CONTROLS.md §6.6). When an Identify is rejected, ask the phone
(`idevicesyslog -u <udid> -p accessoryd`); do not bisect.


## docs/wireless/00_WIRELESS_CARPLAY.md — AirPlay-tunnel iAP2 handshake, 2026-07-24

### R-39-1 · The AirPlayTunnel arm's declared metadata set is app-pushed, not box-selected

- **Verdict:** CORRECTED
- **Landed:** `feat: app-driven doctrine (docs/carplay/04_CAPABILITIES_AND_CONFIG.md) + workstreams A-E` (`f3fa61d`, 2026-08-10)
- **Scope:** §The fix — the `TransportComponent::AirPlayTunnel` bullet
- **Migration note:** The parenthetical recording the byte-pin test's 2026-07-31 rename is carried inside this text; it was added later (`Docs QC: batch-fix the remaining audit findings across 51 files`, `a08b0eb`, 2026-08-16) but is an anchor fix, not a change of claim.

**CORRECTED 2026-08-10 — app-driven doctrine (docs/carplay/04_CAPABILITIES_AND_CONFIG.md).** This doc fixes the AirPlayTunnel arm's
metadata declaration set in box source (params 6/7 pinned by
`ident_info_airplay_tunnel_declares_the_generated_metadata_ids` — the test was renamed from
`ident_info_airplay_tunnel_declares_full_metadata_ids` on 2026-07-31 when the id lists became
`features.rs`-generated). Per docs/carplay/04_CAPABILITIES_AND_CONFIG.md the declared content/tier is
selected by app-pushed config — the box `features.rs` table remains only the generation mechanism.
The handshake mechanics themselves (link + auth + Identify) are permanently-stable box-side
plumbing and are unaffected. The historical record below is unchanged.

### R-39-2 · docs/carplay/03_SDK_GROUND_TRUTH.md: the premise is confirmed, but the inbound feed is wired to the wrong channel

- **Verdict:** PARTIALLY SUPERSEDED
- **Landed:** `docs/carplay/03_SDK_GROUND_TRUTH.md: correct docs/wireless/00_WIRELESS_CARPLAY.md against the licensed R14G17 SDK source` (`aefe6f0`, 2026-07-25)
- **Scope:** §The fix — `iap_tunnel.rs` and the `events.rs` wiring

**PARTIALLY SUPERSEDED — see `../carplay/03_SDK_GROUND_TRUTH.md`.** The premise is CONFIRMED correct by the SDK, but the inbound feed is wired to the wrong channel (docs/carplay/03_SDK_GROUND_TRUTH.md §1), the link choreography is inherited from bt_driver.rs rather than reference-backed (docs/carplay/03_SDK_GROUND_TRUTH.md §7), and the guide's Zero-Ack recommendation is unfollowed (docs/carplay/03_SDK_GROUND_TRUTH.md §8). The "too early" concern raised later was wrong — timing matches `started_f`.
docs/carplay/03_SDK_GROUND_TRUTH.md is derived by reading Apple's licensed R14G17 accessory SDK source directly; the conclusions
corrected there were derived by inference. Where the two disagree, docs/carplay/03_SDK_GROUND_TRUTH.md is correct.

### R-39-3 · Transport premise superseded by docs/carplay/05_METADATA_AND_CONTROLS.md

- **Verdict:** PARTIALLY SUPERSEDED
- **Landed:** `Baseline: commit the 2026-07-29/30 review + Simulator-verification work` (`f88989d`, 2026-07-31)
- **Scope:** Whole document — the transport premise only

Transport premise superseded by `../carplay/05_METADATA_AND_CONTROLS.md`: wireless
iAP2 rides a RemoteControlSession DataStream (SETUP stream type 130), not `iAPSendMessage`
inside `POST /command`. Message-shape and link-layer content here remains valid.


## docs/wireless/01_BT_AND_RADIO.md — Bluetooth bring-up: chip health-check + bt_on.sh race, 2026-07-24

### R-40-1 · The deployed `attach_bluetooth.sh` md5 is superseded — a box still matching it is pre-docs/wireless/01_BT_AND_RADIO.md

- **Verdict:** SUPERSEDED
- **Landed:** `Docs QC: batch-fix the remaining audit findings across 51 files` (`a08b0eb`, 2026-08-16)
- **Scope:** §Deployed — the `attach_bluetooth.sh` md5 only; `bt_on.sh`'s is still current

**SUPERSEDED 2026-08-16 (`attach_bluetooth.sh` only).** That md5 was correct on 2026-07-24 and is
kept as the record of what this fix deployed. The file changed the very next day — docs/wireless/01_BT_AND_RADIO.md removed
the trailing raw `HCI_Reset` and added the `down`/`up` after the MAC-programming `reset` (shipped in
`6425a7a`, the only commit to touch it since) — so a box still matching `642b9a27…` is running the
**pre-docs/wireless/01_BT_AND_RADIO.md** script. That box is not necessarily unable to pair: `carplay-wireless`'s
`bt_bringup::bring_up` now forces its own DOWN→UP, which is the one operation that re-runs the
kernel's init-time `Set_Event_Mask` and restores the SSP events the stray reset wiped, and the
supervisor starts it immediately after BT attach. What the stale script still costs is the vendor
commands the reset undoes (`scomtu`, SCO routing, BLE power) and any attach path that is *not*
followed by `carplay-wireless` coming up behind it. Push the current file rather than reason about
which. `bt_on.sh`'s md5 is still current at HEAD.

## docs/wireless/01_BT_AND_RADIO.md — The "Pairing Unsuccessful" regression: a raw HCI_Reset wiping the SSP event mask, 2026-07-25

### R-41-1 · The "pending live-hardware confirmation" caveat is DOWNGRADED, not retired

- **Verdict:** CORRECTED — caveat strength only; the fix and its analysis are unchanged
- **Landed:** `Docs QC: batch-fix the remaining audit findings across 51 files` (`a08b0eb`, 2026-08-16)
- **Scope:** the document's own status header — "Live-hardware confirmation: strong indirect evidence only"

**UPDATED 2026-08-16 — the "pending live-hardware confirmation" caveat is downgraded, not
retired.** docs/wireless/01_BT_AND_RADIO.md §6b records a full wireless CarPlay session (association → HEVC A/V → 100+
iAP2 metadata messages) on an RTL8822CS unit on 2026-08-15, with `bt_bringup::bring_up`'s DOWN→UP
in place. Wireless CarPlay cannot start without a Bluetooth link first, and that unit's controller
address is its own Realtek efuse (docs/wireless/01_BT_AND_RADIO.md §6c) — an address the test iPhone held no bond for — so
the session almost certainly included a **fresh SSP pairing**, the exact case this document says
was broken. That is inference from a working session, not a logged pairing: §6b captured no mgmt
events. What would close it outright is one `IO_Capability_Request` observed on a first pair.
Scope it honestly either way: that unit runs the mapped Realtek path in `radio_hal.sh`, which
never calls `attach_bluetooth.sh`, so what was exercised is the **daemon-side** half of the fix
(`bt_bringup.rs`) — by design the half that makes the behaviour independent of script history.
The `attach_bluetooth.sh` half remains reviewed-not-retested on IW416.

---

## docs/carplay/03_SDK_GROUND_TRUTH.md — SDK Conformance Corrections: what docs/wireless/00_WIRELESS_CARPLAY.md got wrong, 2026-07-25

### R-43-1 · The `CARPLAY_WIRELESS_METADATA` endorsement is superseded as a design position by the app-driven doctrine

- **Verdict:** CORRECTED — doctrine
- **Landed:** `feat: app-driven doctrine (docs/carplay/04_CAPABILITIES_AND_CONFIG.md) + workstreams A-E` (`f3fa61d`, 2026-08-10)
- **Scope:** the env-var endorsement in §4 and §8 ("Functionally equivalent given one `airplayd` per transport")

**CORRECTED 2026-08-10 — app-driven doctrine (docs/carplay/04_CAPABILITIES_AND_CONFIG.md).** This doc's endorsement of the
`CARPLAY_WIRELESS_METADATA` env var as the stand-in for Apple's transport-type gate —
"Functionally equivalent given one `airplayd` per transport" — is superseded as a design position:
per docs/carplay/04_CAPABILITIES_AND_CONFIG.md, env/`/tmp` levers are interim, subordinate mechanics and the app-pushed config is the
control. The SDK-conformance corrections themselves stand. The historical record below is
unchanged.

### R-43-2 · The wireless-transport conclusions are refuted (docs/carplay/05_METADATA_AND_CONTROLS.md), and §3's practical implication is HALF retracted

- **Verdict:** PARTIALLY SUPERSEDED
- **Landed:** `Baseline: commit the 2026-07-29/30 review + Simulator-verification work` (`f88989d`, 2026-07-31); the §3 half-retraction sentence was added later by `Docs QC: half-retract docs/carplay/03_SDK_GROUND_TRUTH.md §3, and drop a confounded device claim` (`cfa6447`, 2026-08-16)
- **Scope:** §1, §3, §4, §8.1 — and the document-wide transport premise

Where this document and docs/wireless/00_WIRELESS_CARPLAY.md disagree on **R14G17's contents**, it remains correct. **But its
conclusions about the wireless transport are refuted**, because R14G17 is a 2017 drop silent on the
DataStream/RCS layer — and this document read that silence as an answer. Specifically: **§1's "the
actual reason the tunnel never worked" is not the actual reason**; **§4's channel-asymmetry framing is
scoped to the wrong carrier**; **§8.1's rejection of `0xFFFF` was wrong and was cited to block the fix
that was needed.** **§3's practical implication ("almost certainly inert extra keys") is half
retracted 2026-08-16 — the `"iAPChannel"` echo IS load-bearing (CarPlaySDK 509.11 + iOS 27
`AirPlaySender`); its R14G17 grep findings and its rejection of docs/carplay/05_METADATA_AND_CONTROLS.md §1.1's 400 causal chain both
stand.** Its `data`-key, reply-shape and Zero-Ack findings stand.

---

## docs/carplay/02_SESSION_LIFECYCLE.md — Session-start ordering: when the iAP2 tunnel may be opened, and which references are normative, 2026-07-25

### R-44-1 · §7.5's acceptance of the `CARPLAY_WIRELESS_METADATA` proxy is superseded as a design position

- **Verdict:** CORRECTED — doctrine
- **Landed:** `feat: app-driven doctrine (docs/carplay/04_CAPABILITIES_AND_CONFIG.md) + workstreams A-E` (`f3fa61d`, 2026-08-10)
- **Scope:** §7.5 (the "Same intent, different mechanism" divergence); §1 explicitly untouched

**CORRECTED 2026-08-10 — app-driven doctrine (docs/carplay/04_CAPABILITIES_AND_CONFIG.md).** §7.5's acceptance of the process-scoped
`CARPLAY_WIRELESS_METADATA` env var as the wireless proxy ("Same intent, different mechanism") is
superseded as a design position — on-box levers are interim scaffolding, with the app-pushed config
primary per docs/carplay/04_CAPABILITIES_AND_CONFIG.md. §1's reference-authority order is untouched and remains the owner directive.
The historical record below is unchanged.

### R-44-2 · The transport premise throughout is refuted; §1 stands with one amendment

- **Verdict:** PARTIALLY SUPERSEDED
- **Landed:** `Baseline: commit the 2026-07-29/30 review + Simulator-verification work` (`f88989d`, 2026-07-31)
- **Scope:** §1 (amendment), §2, §4, §7.1 — and the document-wide transport premise; §3.1 and §5 unaffected

The **reference authority order (§1) stands**, with one amendment: §1's SpeedPlay demotion was too
broad (docs/carplay/05_METADATA_AND_CONTROLS.md §8). The **transport premise throughout is refuted** — wireless iAP2 rides a
RemoteControlSession DataStream (SETUP stream type 130), not `iAPSendMessage` in `POST /command`.
§2's ordering change is conformant and was verified on hardware, but it did **not** fix metadata;
ordering was never the blocker. **§4's Zero-Ack row is actively wrong and cost a deploy cycle** — see
the inline correction. §7.1 is now answered. §3.1 (`disableBluetooth`) and §5 (`AdvancedFeatures`)
stand.

### R-44-3 · §1's original 2026-07-25 authority order, superseded by the 2026-08-10 reorder

- **Verdict:** SUPERSEDED
- **Landed:** `docs: reorder the reference-authority chain by CURRENCY (owner directive)` (`de02ba1`, 2026-08-10)
- **Scope:** §1 — the 2026-07-25 ordering, kept here for provenance

**⚠️ SUPERSEDED 2026-08-10 by a further owner directive — see the REORDERED list below.** The
2026-07-25 order (kept here for provenance) read:
*1. R14G17 + the Simulator (normative) · 2. CT5 CINEMO · 3. everything else.*
It bundled the 2017 source with the current Simulator as one tier, and that bundling is exactly what
let a 2017 drop's SILENCE be read as an answer about features added after it.

---

## docs/carplay/05_METADATA_AND_CONTROLS.md — Wireless iAP2 transport: the RCS DataStream, 2026-07-25

### R-45-1 · The document's present-tense OPERATIONAL claims are stale — the channel regressed 2026-07-31 → 08-10

- **Verdict:** STALE — operational status and implementation state only; the protocol findings are unaffected
- **Landed:** `fix(wireless): stop refusing iOS's type-130 DataStream SETUP — the dead metadata plane` (`9a5e38e`, 2026-08-10)
- **Scope:** every present-tense "works / operates / confirmed in service" claim; see §8

**⚠️ Status correction, 2026-08-10.** Read every present-tense "works / operates / confirmed in
service" claim in this document as **"proven once on 2026-07-25, then regressed on 2026-07-31"**.
The channel was dead from 2026-07-31 to 2026-08-10 (§8). The protocol facts below are unaffected and
remain correct — they are what made the diagnosis possible. What is stale is the *operational
status*, and, importantly, the implementation: the code that produced the 07-25 result was never
committed in that form. See §8 before trusting any claim here about what currently runs.

---

## docs/ops/02_TESTING.md — Live session test plan: verifying the RCS `'cmnd'` fix, 2026-07-25

### R-46-1 · The three `ctrl=`/`TX(RCS,'cmnd')` discriminator greps are DEAD — they read 0 on a healthy box

- **Verdict:** CORRECTED
- **Landed:** `Docs QC: three more dead greps, and six status-ledger items that shipped` (`28f9fdd`, 2026-08-16)
- **Scope:** "The discriminator" section — the `ctrl=0x40` / `ctrl=0xc0` greps; also referenced by pre-flight gate 2 and the decision tree

**⚠️ CORRECTED 2026-08-16 — the three greps below are DEAD.** `[datastream] RX … ctrl=…` (`session.rs`, the per-frame RX log) and `TX(RCS,'cmnd')` (`datastream.rs`) are both inside `if crate::events::events_log()`, gated by `a46098c` (2026-07-31) *after* this plan was written, and `CARPLAY_EVENTS_LOG` is set by **no spawn site** — it is only ever read. So every one of them reads 0 on a HEALTHY box, and a 0 on the `ctrl=0xc0` line reads as *better* than "accepted". Use the ungated line instead: `grep -c 'outbound sink registered' $L` ≥ 1 proves the RCS sink was installed (`datastream.rs`), and `grep -c 'SETUP phase2 DataStream(130)' $L` proves the phone asked. To revive the byte-level greps, set `CARPLAY_EVENTS_LOG=1` in the airplayd spawn env first.

### R-46-2 · The decision tree's `grep -c "SETUP stream type=130"` can never match

- **Verdict:** CORRECTED
- **Landed:** `Docs QC: batch-fix the remaining audit findings across 51 files` (`a08b0eb`, 2026-08-16)
- **Scope:** the decision-tree row "No stream-130 SETUP at all"

**CORRECTED 2026-08-16.** The grep this row used to prescribe, `grep -c "SETUP stream type=130"`,
can never match: the scid guard that emitted that line was narrowed to `100..=102 | 110 | 111` when
the regression was fixed (`session.rs`, the `scid == 0 && matches!(ty, …)` arm), so 130 is exempt and
the count is permanently 0 — which this row then reads as "the phone never asked".

---

## docs/carplay/05_METADATA_AND_CONTROLS.md — The CarPlay metadata surface: what exists, what we ask for, what arrives (2026-07-25)

### R-47-1 · "One defect remains" was true on the day and false for the ten days after

- **Verdict:** CORRECTED
- **Landed:** `fix(wireless): stop refusing iOS's type-130 DataStream SETUP — the dead metadata plane` (`9a5e38e`, 2026-08-10)
- **Scope:** the "one defect remains" line in the preamble

**⚠️ Corrected 2026-08-10 — "one defect remains" was true on 2026-07-25 and false for the ten days
after.** A second, unrelated defect existed from 2026-07-31: the wireless TRANSPORT under this
surface was broken (docs/carplay/05_METADATA_AND_CONTROLS.md §8), so on wireless the records did not reach the app at all. It is
outside this document's subject — the declaration/subscribe conclusions here still stand, and the
host-side pane defect is still open — but do not read this line as "wireless metadata is otherwise
healthy". Verify the transport is alive before debugging anything in §6.6 on a wireless session.

### R-47-2 · Re-measured at tier `extended` — the table in §3 is now the OLD floor

- **Verdict:** CORRECTED
- **Landed:** `docs: `extended` accepted on the tunnel arm — measured, and the how-to recorded` (`2e39bab`, 2026-08-10)
- **Scope:** §3's measurement table

**RE-MEASURED 2026-08-10 at tier `extended`, and the table above is now the OLD floor.** Same
hardware, same arm, fresh session: Identify **342 B**, `RX 0x1D02` accepted (0 rejects),
NowPlaying **388 and climbing** in ~90 s, album artwork **2** transfers, and — the point of the
tier — **`0x4158 CommunicationsUpdate` = 2**, a feed this document measured at **0** under the
declaration in §3 because `proven` does not declare it. `0xAE01 PowerUpdate` = 0, which is expected
rather than a defect: it is declared, and the phone sends it on power-state changes only.

`extended` is now device-proven TWICE on the AirPlayTunnel arm (2026-07-25 §6.6 at 340 B, and
2026-08-10 at 342 B — the 2 B delta is accessory-name length). It is the largest declaration this
hardware has been observed to accept; `all` is refuted (§ the tier-`all` box above).

Compiled default remains `proven` pending an owner decision — that is a doctrine question
(docs/carplay/04_CAPABILITIES_AND_CONFIG.md: the tier is app-owned), not a measurement question, and the measurement is not by itself
a reason to move the FLOOR, which exists to be the recovery baseline.

### R-47-3 · Tier `all` is REFUTED on the AirPlayTunnel arm, and iterative skipping is a dead end

- **Verdict:** REFUTED
- **Landed:** `docs: record the tier-`all` tunnel Identify reject (device evidence)` (`8685824`, 2026-08-10)
- **Scope:** §3 / the tier levers

**⚠️ DEVICE EVIDENCE, 2026-08-10 — tier `all` is REFUTED on the AirPlayTunnel arm, and the phone
named the ids.** A live tunnel Identify carrying tier `all` drew a decoded `0x1D03`:
*param 6 unsupported: `0x560F StartVoiceOverCursorUpdates`, `0x5611 StopVoiceOverCursorUpdates`;
param 7 unsupported: `0x5610 VoiceOverCursorUpdate`.* The retry stripped [6,7] and was
BYTE-IDENTICAL (they are `REQUIRED_IDENT_PARAMS`), so the second reject aborted the tunnel —
the unrecoverable-within-a-session behaviour this project documents, observed end to end for the
first time.

Three consequences. (1) Only ONE feature is refused, not the tier as a class: those three ids are
exactly `features.rs`'s `voice_over_cursor`, so `tier: all` + `skip: [voice_over_cursor]` should
pass — the existing lever, no code change, rollback by config push. NOT yet device-verified; it is
the obvious next experiment — **TRIED THE SAME NIGHT AND REFUTED: the skip removed the ids (372 B
-> 366 B, exactly three ids) but iOS rejected anyway with a GENERIC `param 6 (no detail)`. So `all`
is refused for more than `voice_over_cursor`, and because the phone stops enumerating once its
specific objection is satisfied, iterative skipping is a dead end. Treat `all` as REFUTED on this
arm.** (2) This says nothing against `extended`, which CLAUDE.md records as
accepted on this arm 2026-07-25. (3) It was only VISIBLE because the type-130 DataStream fix
restored the inbound path the same day — before that the tunnel sat at `Init` and the reject never
arrived, which is how a declaration problem spent ten days looking like a transport problem.
Full capture: `docs/ops/captures/2026-08-10_TUNNEL_IDENT_REJECT_tier_all_voiceover_cursor.txt`.

### R-47-4 · "Arrives" / "works" describe the 2026-07-25 session only; the tunnel was dead 07-31 → 08-10

- **Verdict:** CORRECTED
- **Landed:** `fix(wireless): stop refusing iOS's type-130 DataStream SETUP — the dead metadata plane` (`9a5e38e`, 2026-08-10)
- **Scope:** §6 — every "arrives" / "works" claim

**⚠️ Status correction, 2026-08-10.** "Arrive" / "works" here describe the 2026-07-25 session. The
RCS tunnel was dead from 2026-07-31 to 2026-08-10 (docs/carplay/05_METADATA_AND_CONTROLS.md §8 — a scid guard rejected the type-130
SETUP), so NOTHING arrived over it in that window: no NowPlaying, no RouteGuidance, no CallState, no
artwork. The declaration/subscribe findings in this document are unaffected and remain correct — the
failure was one layer below, in the transport that carries them. Wired metadata was unaffected
throughout.

## docs/ops/05_AUDITS.md-agent code review with corroboration, 2026-07-29

### R-48-1 · §8's framing of `/tmp/carplay_metadata` as the canonical operator control is superseded

- **Verdict:** CORRECTED — doctrine
- **Landed:** `feat: app-driven doctrine (docs/carplay/04_CAPABILITIES_AND_CONFIG.md) + workstreams A-E` (`f3fa61d`, 2026-08-10)
- **Scope:** §8 — the lever framing and the orphaned-rider footgun class it documents

**CORRECTED 2026-08-10 — app-driven doctrine (docs/carplay/04_CAPABILITIES_AND_CONFIG.md).** §8's framing of the
`/tmp/carplay_metadata` lever as the canonical control an operator reaches for to shape the
declaration is superseded: per docs/carplay/04_CAPABILITIES_AND_CONFIG.md the levers are interim, box-side scaffolding pending
migration to app-pushed config (which also removes the orphaned-rider footgun class §8 documents,
since the app validates before pushing). The review findings and landed fixes themselves stand.
The historical record below is unchanged.

## docs/carplay/03_SDK_GROUND_TRUTH.md — CarPlay Simulator verification pass, 2026-07-30

### R-49-1 · §8 / §11 `/info` emission policy is app-owned, not box-owned

- **Verdict:** CORRECTED — doctrine
- **Landed:** `feat: app-driven doctrine (docs/carplay/04_CAPABILITIES_AND_CONFIG.md) + workstreams A-E` (`f3fa61d`, 2026-08-10)
- **Scope:** §8, §11

§8/§11's box-owned `/info` emission policy and the plan to tune the param-30 storage capacities
box-side are superseded: `/info` content and capacity values are app-authored and pushed,
box-framed, per docs/carplay/04_CAPABILITIES_AND_CONFIG.md.

Note that §4's finding — the app already writes `rightHandDrive`/`nightMode` but the box drops
them — is doctrine-SUPPORTING: the doctrine's fix is to parse and honor the app's values. The
historical record in the document is unchanged.

### R-49-2 · §5's central claim is refuted — cluster content works via `showUI` query parameters

- **Verdict:** REFUTED (central claim); the factual inventory survives
- **Landed:** `fix(app): the alt-ON document did not parse — a glued initialURL line` (`392989b`, 2026-08-11)
- **Scope:** §5 — "Alt / cluster video — root cause"

§5 says our flat `displays[]` is "sufficient to negotiate and receive the type-111 stream and
**structurally incapable of defining anything inside it**", and names the missing `/info`
`displayPanels[]` array as the alt-content ROOT CAUSE.

**Cluster content works, and its elements are toggleable** — owner-confirmed on hardware
2026-08-11, running. The mechanism is `showUI` with QUERY PARAMETERS, not `displayPanels[]`:

- `ClusterContent` — None / Instruction Card / Map / Navigation App (`ControlsWindow.swift`,
  the `ClusterContent` enum), each selecting a `maps:/car/instrumentcluster/...` URL;
- `showSpeedLimit` / `showCompass` / `showETA` (+ `maneuverLayout`) carried as query flags on
  that URL — `NAV_APPEARANCE_*` in `ocbm-proto`, built in `ccpa/airplayd/src/main.rs`, whose own
  comment calls them "literally 'the elements inside the navigation video'";
- and the three advertised cluster URLs (map + instructioncard + base) that let iOS composite
  the maneuver card at all — advertising only `/map` had previously told iOS the cluster was
  map-only.

That vocabulary was read off Apple's own Simulator (`AirPlayShowUIURL.airPlayURL`), so it is
Apple's mechanism rather than a workaround. The section's own caveat — "no capture exists of any
device successfully driving cluster content" — was true when written and is now false.

**WHAT SURVIVES:** the factual inventory in §5 (CarPlaySDK emits both `displays` and
`displayPanels`; the ~19-key panel dict; `legacyDisplayInfo` as the toggle; `DisplayPanelProperty`
having exactly three cases) is verified and still correct. What does NOT survive is the CAUSAL
claim that `displayPanels[]` is required to control cluster content. Anyone reviving that work
must first establish what it adds BEYOND `showUI` — see §8, "Open, in priority order".

### R-49-3 · The "secondary gaps are all implemented" claim was an over-claim

- **Verdict:** CORRECTED — supersedes part of `R-49-2`'s annotation
- **Landed:** `docs: correct my own over-claim in the docs/carplay/03_SDK_GROUND_TRUTH.md annotation + scope the drift fixture honestly` (`ac92109`, 2026-08-11 00:10)
- **Scope:** §5, the "secondary gaps" note

An earlier version of the §5 annotation said the secondary gaps were "all implemented", which
was itself an over-claim. Corrected to:

- The SENDERS exist (`events.rs` `mapAppearanceUpdate` / `changeMapZoomLevel` /
  `uiAppearanceUpdate`) and `/info` emits the appearance keys (`info.rs`) — but it emits them
  UNCONDITIONALLY.
- `enablesMapAppearance` is genuinely NOT PARSED. Verified: `AccessoryConfig` parses exactly SIX
  keys (`enablesHEVC`, `enablesViewAreas`, `enablesCornerMasks`, `enablesLogTransfer`,
  `enablesMainBufferedAudio`, `appDrivenSetup`) while the app emits SIXTEEN. **Ten are
  discarded.** So the capability is BOX-decided, which is the docs/carplay/04_CAPABILITIES_AND_CONFIG.md directive-2 violation —
  not that the feature is missing.

**This entry was itself overtaken 95 minutes later — see `R-49-4`.** It is
kept because it is the direct ancestor of the fix: the commit that superseded it restates this
argument almost verbatim in its new field's doc comment.

### R-49-4 · The app now owns `uiAppearance`, `mapAppearance` and `focusTransfer`

- **Verdict:** SUPERSEDES `R-49-3` (95 minutes later)
- **Landed:** `fix(doctrine): the app now owns uiAppearance, mapAppearance and focusTransfer` (`a35d743`, 2026-08-11 01:45)
- **Scope:** §5

`R-49-3`'s two bullets were correct when written and are left standing rather than rewritten.
What changed:

- `AccessoryConfig` (`crates/vendor/receiver/src/vehicle_config.rs`, `struct AccessoryConfig`)
  now parses **NINE** keys — the six named plus `enablesUIAppearance`, `enablesMapAppearance`,
  `enablesFocusTransfer` — all nine armed per control connection in `ccpa/airplayd/src/main.rs`,
  `fn load_device_config` (called per connection).
- `/info` no longer emits the appearance keys unconditionally:
  `crates/vendor/receiver/src/info.rs`, `fn add_appearance_keys`, gates each pair on
  `levers::ui_appearance()` / `levers::map_appearance()`. Both default `true`
  (`levers.rs`, `static UI_APPEARANCE` / `static MAP_APPEARANCE`, and BOTH
  `AccessoryConfig::default` and the per-field `default_true` — the two are required
  separately), so an unconfigured box is byte-identical; only an owner turning a toggle **off**
  changes the wire.
- **SEVEN of sixteen are discarded, not ten:** `enablesVideoPlayback`, `enablesEnhancedSiri`,
  `enablesUIContext`, `enablesUISync`, `enablesFileTransfer`, `enablesVehicleDataProtocol`,
  `enablesDCX`. All seven name capabilities the box does not implement **at all**, so they are
  an unimplemented-feature backlog, NOT a doctrine violation — extending the serde gate alone
  would buy nothing. The set is pinned by the drift guard
  `every_emitted_key_is_parsed_or_knowingly_ignored` (`vehicle_config.rs`), so it cannot
  silently drift again.
- **What the doctrine complaint should name INSTEAD, and still should:** the display `features`
  word. `info.rs` is a hardcoded `if levers::dpad() { 0x1A } else { 0x0A }` while the box parses
  `touchpadSupport`, `steeringWheelSupport`, `mediaButtonsSupport` and `touchScreenMode`
  (`vehicle_config.rs`) and acts on none of them; `mediaButtonsSupport` cannot suppress the
  unconditional uid-2 device (`info.rs`, `HID_UID_MEDIA_BUTTONS`). That is the real "parsed, then box-decided" residue,
  knowingly deferred to C-7/C-8. Second residue: `enablesFocusTransfer` reaches `/info`
  (`info.rs`) but `"focusTransfer"` is absent from the SETUP `enabledFeatures` echo on BOTH
  authoring paths (`crates/vendor/receiver/src/session.rs`; host
  `host/CarPlayHost/carlink_macOS/App/VehicleConfig.swift`, `func enabledFeatures()`), so it can never be negotiated —
  advertise-half-armed.
- **Live app bug this created:** `host/CarPlayHost/carlink_macOS/App/SettingsWindow.swift`,
  `inertKeys`, still
  lists `enablesUIAppearance` and `enablesMapAppearance` in `inertKeys`, so their tooltips still
  say the setting "has no effect on the wire". Added 2026-08-10, never removed when `a35d743`
  made them live — the exact mistake the `NOT inert, removed 2026-08-10` comment *inside*
  `inertKeys` warns about. **Still open**;
  tracked as Phase-4 defect 4 in `../ops/04_OPEN_ITEMS.md`.
- `uiContext` IS still missing, verified in code: nothing emits or echoes it, and
  `changeUIContext` appears only as a log-classifier string in the host. Note §5 says "two
  `uiContext*URLs` keys"; CarPlaySDK carries THREE — `uiContextURLs`,
  `uiContextNowOnDisplayURLs`, `uiContextLastOnDisplayURLs`.

Mitigating, and worth stating so the gap is not overstated: the app marks six of the seven
discarded keys inert in its own tooltips, so the owner is warned rather than misled. The
doctrine problem is the unconditional box-side emission, not silent deception.

### R-49-5 · Inert-key counts in `R-49-4` corrected

- **Verdict:** CORRECTED — counts only
- **Landed:** `Docs QC: three more dead greps, and six status-ledger items that shipped` (`28f9fdd`, 2026-08-16)
- **Scope:** §5, the mitigating note in `R-49-4`

The mitigating note originally read "the app marks nine of the ten discarded keys inert". With
the discarded set corrected to seven, the true count is **SIX of the SEVEN** — verified against
`inertKeys` in `host/CarPlayHost/carlink_macOS/App/SettingsWindow.swift`. `R-49-4` above carries
the corrected wording.

### R-49-6 · `enablesEnhancedSiri` DOES ship a warning — `R-49-5` overstated the gap

- **Verdict:** CORRECTED — supersedes the second half of `R-49-5`
- **Landed:** this entry (docs-only; see `Docs: fix the pilot migration's fabricated anchors and half-migrated state`)
- **Scope:** §5

`R-49-5` as first written added "…and ships with no ⚠️ marker" to the `enablesEnhancedSiri`
finding. **That half is false.** The key's description in `SettingsWindow.swift` carries a
hand-written ⚠️ — *"⚠️ Out of scope — not pursued …; the box does not declare enhancedSiri, so
this toggle has no effect"* — added in `ca5193a1` (2026-08-07), well before the claim was made.

The accurate statement is narrower: `enablesEnhancedSiri` is absent from the `inertKeys` set, so
it gets no *automatic* inert marker, and is warned about only by hand-written prose. That is
still a real defect — the file's own comment says the hand-written route is the one that gets
missed — but it is "warned by the fragile mechanism", not "not warned at all".

The false half was inherited verbatim from `28f9fdd` and then promoted into this ledger without
re-verification, which is precisely the drift the split exists to prevent. Caught by the
independent validation of the pilot migration. Tracked as Phase-4 defect 4 in
`../ops/04_OPEN_ITEMS.md`, which stated it correctly throughout.

### R-49-7 · §10's "no regression" outcome is FALSE — the pass killed wireless metadata for ten days

- **Verdict:** REFUTED
- **Landed:** `feat: app-driven doctrine (docs/carplay/04_CAPABILITIES_AND_CONFIG.md) + workstreams A-E` (`f3fa61d`, 2026-08-10)
- **Scope:** §10 — "Outcome of the fixes — one refuted on hardware"

**This pass DID cause a regression, and it killed the entire wireless metadata plane for ten
days (2026-07-31 → 08-10).** The `streamConnectionID=0 rejection` credited to this pass in a
commit message — but described nowhere in docs/carplay/03_SDK_GROUND_TRUTH.md, §7 included — was applied to
EVERY stream type.

*(The original annotation cited that commit as 5ce9d1c — written here WITHOUT backticks
because it **resolves in no ref**, and the backtick form is reserved for SHAs that must
resolve (`tools/docs_status_check.py` enforces exactly that): it
predates the `18ba44b` squash and did not survive it. The guard reaches current history inside
`Baseline: commit the 2026-07-29/30 review + Simulator-verification work` (`f88989d`,
2026-07-31), and the fix is `fix(wireless): stop refusing iOS's type-130 DataStream SETUP — the
dead metadata plane` (`9a5e38e`, 2026-08-10). Found by `git log -S"streamConnectionID"` on
`session.rs`; the dead hash was flagged by `tools/docs_status_check.py`.)* Type 130 (the RCS DataStream that carries wireless iAP2, docs/carplay/05_METADATA_AND_CONTROLS.md)
legitimately arrives with no `streamConnectionID`, so every one was skipped before reaching its
arm, no `streamID` transport token was returned, and the phone's outbound iAP2 path never
existed. The tunnel sat at `Init` and NOT ONE metadata record arrived.

§10's "A/V and metadata flow" was true of A/V only. The wireless metadata check was not actually
performed; A/V health was read as whole-session health. That is the specific mistake — the two
planes fail independently, and this one fails silently.

Evidence and fix: `docs/ops/captures/2026-08-10_REGRESSION_datastream130_scid_rejected.txt`; the
guard is now an allowlist (`crates/vendor/receiver/src/session.rs`), regression-tested at
`crates/vendor/receiver/tests/setup_stream_130.rs`.

**Process lesson, and the reason docs/carplay/03_SDK_GROUND_TRUTH.md is annotated rather than edited:** the change was never
written down. It appears only in a commit message, so ten subsequent commits to `session.rs`
reviewed it without ever seeing a stated scope. An undocumented hardening is unreviewable.

### R-49-8 · §8.1's "`lane_guidance` is inert / `0x5204` can never arrive" is REFUTED by our own capture

- **Verdict:** REFUTED
- **Landed:** `Docs: ../ops/04_OPEN_ITEMS.md - one index of what is actually still outstanding` (`6da533e`, 2026-08-16)
- **Scope:** §8 item 1; the same claim in `crates/vendor/iap2-core/src/message.rs`

§8.1 reasoned from Apple's wording for Identify param 30 sub 8 `MaxLaneGuidanceStorageCapacity`
— *"Must be included to receive Lane Guidance instructions."* — that because we never send sub 8,
`0x5204` can never arrive "regardless of tier or subscribe", and that `features::lane_guidance`
is therefore inert.

**The 2026-07-29 session delivered `0x5204 LaneGuidanceInformation` ×12**, alongside `0x5201`
×574 — recorded in docs/ops/05_AUDITS.md under "`0x5204` confirmed via `RidesOn`", and described there as the
first live confirmation that a subscribe-less `Trigger::RidesOn` feature receives data. Lane
guidance has no `Start*` of its own: it is declared in param 7 and rides `route_guidance`'s
subscribe.

So the feature is **not** inert and sub 8 is not the gate it was read as. **What sub 8 actually
controls is not established.** Do not substitute a replacement theory without a capture — reading
a documented sentence as a mechanism, with no wire evidence, is what produced the wrong claim in
the first place.

Two notes on scope. The **param-30 expansion is still genuinely open** (see
[../ops/04_OPEN_ITEMS.md](../ops/04_OPEN_ITEMS.md)); only the reason given for it here is wrong. And the adjacent
claim about `metadata.rs` parsing `CurrentRoadName`/`DestinationName` while subs 2 and 3 are
absent is **not** refuted by this capture — it was not tested by it.

The claim had propagated into a source comment (`message.rs`, the param-30 block), which is why
this entry exists rather than a silent edit: the document and the code asserted it in the same
words, and the capture refuting it had been sitting in docs/ops/05_AUDITS.md since the day after it was written.


## docs/ops/05_AUDITS.md — Full-Codebase Audit & Remediation — 2026-07-31

### R-50-1 · Finding A's box-autoconnect fix conflicts with the app-driven doctrine — and was never landed anyway

- **Verdict:** CORRECTED — doctrine
- **Landed:** `feat: app-driven doctrine (docs/carplay/04_CAPABILITIES_AND_CONFIG.md) + workstreams A-E` (`f3fa61d`, 2026-08-10)
- **Scope:** Finding A (the on-hardware validation section, "A. The box never BT-autoconnects a known device"); the rest of the audit is unaffected

**CORRECTED 2026-08-10 — app-driven doctrine (docs/carplay/04_CAPABILITIES_AND_CONFIG.md).** Finding A's box-autoconnect fix
(the box BT-paging a known device on its own) institutionalizes box-initiated bring-up, which
conflicts with docs/carplay/04_CAPABILITIES_AND_CONFIG.md directive 3 (box holds IDLE until the app connects and pushes config). The
behavior remains shipped until re-gating lands; the tension recorded in docs/carplay/04_CAPABILITIES_AND_CONFIG.md was RESOLVED
2026-08-10 by owner directive (radios power on only on app command, off on app command or loss of
the app connection; auto-connect once on is correct; page-on-boot SUPERSEDED — re-gating is an
open implementation task, landed in-repo 2026-08-10 with deploy + hardware validation pending —
and this doc's Finding-A mgmt-Add-Device fix was verified NEVER LANDED in code: the docs/wireless/01_BT_AND_RADIO.md
Model-B loop superseded it, so there was nothing to un-ship). The
rest of the audit's findings and remediations are unaffected. The historical record below is
unchanged.

### R-50-2 · The "Phase 2 — planned (not done)" section had in fact all shipped

- **Verdict:** SHIPPED
- **Landed:** `Docs QC: three more dead greps, and six status-ledger items that shipped` (`28f9fdd`, 2026-08-16)
- **Scope:** "Phase 2 — planned (not done)" and its four sub-sections
- **Note:** The code landed across several commits over 2026-08-01 → 08-02; only `5756f36` is named in the moved text, and it resolves. `28f9fdd` is the commit that established and recorded the shipped state.

Everything in this section shipped. The desk-side batch: `frame_into`/`try_frame_into`
(`crates/ocbm-proto/src/lib.rs`), the `OutQueue` cursor (`ccpa/ocbmd/src/main.rs`), and
`enum SendOutcome { sent, droppedNotSubscribed, writeFailed }` — verbatim the enum this section says
still needs writing — at `OCBMClient.swift`. `OCBMSessionCoordinator.swift` was split out, the USB
C1/C4 items are tagged in `USBTransport.swift`, and the legacy `Protocol/` layer was deleted in
`5756f36` (2026-08-01). Text below kept as the record of what was planned.


## docs/wireless/01_BT_AND_RADIO.md — Accessory-Initiated Wireless CarPlay Reconnect — Investigation & Model-B Plan (2026-08-01)

### R-51-1 · Box-autonomous page-on-boot conflicts with docs/carplay/04_CAPABILITIES_AND_CONFIG.md directive 3 — the power-on trigger is superseded

- **Verdict:** SUPERSEDED (the power-on trigger only)
- **Landed:** `feat: app-driven doctrine (docs/carplay/04_CAPABILITIES_AND_CONFIG.md) + workstreams A-E` (`f3fa61d`, 2026-08-10)
- **Scope:** The whole document's premise (box pages the bonded iPhone on boot). The dead-end eliminations and reconnect mechanics are NOT superseded.

**CORRECTED 2026-08-10 — app-driven doctrine (docs/carplay/04_CAPABILITIES_AND_CONFIG.md).** This doc's goal — the box autonomously
paging the bonded iPhone on boot, with no app in the loop — conflicts with docs/carplay/04_CAPABILITIES_AND_CONFIG.md directive 3
(box IDLE until the app connects and pushes config). **RESOLVED 2026-08-10 (owner directive, see
docs/carplay/04_CAPABILITIES_AND_CONFIG.md):** radios power on only on app command; once app-commanded on, auto-connect to
known/bonded devices is correct; radios power off on app command or loss of the app connection.
The shipped page-on-boot behavior is SUPERSEDED (the power-on trigger is incorrect per the
directive; until re-gating lands it remains shipped operational reality, not a sanctioned mode) —
re-gating it on app presence is an
open implementation task. The dead-end eliminations and reconnect mechanics recorded here
stay authoritative — do not re-derive them. The historical record below is unchanged.

### R-51-2 · Re-gating code landed in-repo 2026-08-10; deploy + hardware validation still pending at that date

- **Verdict:** CORRECTED — status of `R-51-1`'s "open implementation task"
- **Landed:** `feat: app-driven doctrine (docs/carplay/04_CAPABILITIES_AND_CONFIG.md) + workstreams A-E` (`f3fa61d`, 2026-08-10)
- **Scope:** The re-gating status recorded against `R-51-1`
- **Note:** Superseded 2026-08-11 by `R-51-3`; kept because the sequence is the record.

**Re-gating status (2026-08-10, workstream A):** code landed in-repo (CT_RADIO kill switch,
hci0-down teardown, supervisor startup reconciliation; the repo boot chain was found to already
gate bring-up on app presence — this doc's paging mechanics run unchanged inside carplay-wireless,
whose spawn trigger IS the app-presence edge). Deploy + hardware validation pending (plan_A §5).
The deployed box was audited live 2026-08-10 (read-only, OCBM console): supervisor md5 matches
repo HEAD (zero drift) and radios are fully down at idle — the deployed box already exhibits
the app-gated behavior; only the new gap fixes (hci0-down teardown, CT_RADIO, reconciliation)
remain to deploy.

### R-51-3 · The re-gating is CLOSED — the guard sits on `wireless_up` itself, and the two call-site strategies that failed must not be retried

- **Verdict:** SUPERSEDES `R-51-1`'s "open implementation task" and `R-51-2`'s "deploy + hardware validation pending"
- **Landed:** `fix(supervisor): guard wireless_up itself — the four bring-up paths are the real problem` (`ecf71e1`, 2026-08-11)
- **Scope:** The re-gating status; the gating strategy for anyone touching it

**CLOSED 2026-08-11 (recorded 2026-08-16) — this supersedes both "open implementation task" and
"deploy + hardware validation pending" above.** The re-gating is landed: the guard sits on
`wireless_up` ITSELF rather than on its four call sites, alongside the CT_RADIO inhibit
(`/tmp/radio_off`, owned by `ocbmd` and set/cleared by the host command), the wired-ownership
choke point, the teardown and the supervisor's startup reconciliation — all in
`tools/session_supervisor.sh`. `../ops/04_OPEN_ITEMS.md` §2 records
`/script/session_supervisor.sh` deployed and matching HEAD **as of 2026-08-11**; that file has
changed since (the radio seam moved its four radio call sites on 2026-08-15), so re-check drift
before assuming the box is current. §3 records the suppression observed live (`SUPPRESSED=1`,
`hostapd=0`, `carplay-wireless=0`, `hci0` reporting "No such device"). **Read that §3 before
touching the gating**: gating on `wired_iphone_on_usb` was tried and made it worse (that predicate
goes false at the `08e4` role switch, so the guard became a self-cancelling oscillator), and
guarding the four call sites individually cannot hold. Residual tracked in §5 (#28):
`wireless_down` orphans the bring-up's forked children.


## docs/carplay/06_AV_PIPELINE.md — CarPlay cornerMasks: protocol, wire format, and how it was cracked (2026-08-02)

### R-52-1 · The receiver-side claim that `safeArea` is always emitted with no exclusion is refuted by the sender-side validator

- **Verdict:** REFUTED
- **Landed:** `docs/carplay/06_AV_PIPELINE.md: CarPlay cornerMasks protocol, wire format, and method` (`99262dd`, 2026-08-02)
- **Scope:** §2 "Mutual exclusion — REAL and per-display"

A prior note (from the *receiver-side* `CarPlaySDK` /info-builder) claimed safeArea is always emitted
and there is no exclusion. That is **refuted** by the *sender-side* validator disasm and by live
hardware (dropping safeArea is what makes it pass). Trust the validator.


## docs/carplay/04_CAPABILITIES_AND_CONFIG.md — CarPlay capability research & implementation roadmap (2026-08-02)

### R-53-1 · Every `vehicle_config.rs` line reference in docs/carplay/04_CAPABILITIES_AND_CONFIG.md is stale, plus two further corrections from docs/carplay/05_METADATA_AND_CONTROLS.md

- **Verdict:** STALE
- **Landed:** `Docs QC: the VehicleConfig parse claims (docs/carplay/03_SDK_GROUND_TRUTH.md, docs/carplay/04_CAPABILITIES_AND_CONFIG.md, docs/carplay/03_SDK_GROUND_TRUTH.md)` (`41ffa3a`, 2026-08-16)
- **Scope:** Whole document — every `vehicle_config.rs` citation; §Mechanism B step (3); §1's focusTransfer lever claim. §8 fileTransfer, §9 uiSync and §10 DCX are explicitly unaffected.

**STALE CODE ANCHORS — read before acting on any `vehicle_config.rs` line reference in this file
(added 2026-08-16).** Every `vehicle_config.rs:232-249` / `:238-249` / `:238/243/248` citation below
was accurate at the 2026-08-02 tree and **none of them resolve today.** `AccessoryConfig` now lives at
`vehicle_config.rs:351-408` and parses **nine** keys, not three — see the STATUS UPDATE blocks at the
TL;DR and at "Shared infrastructure" item 1. The seven still dropped are `enablesVideoPlayback`,
`enablesEnhancedSiri`, `enablesUIContext`, `enablesUISync`, `enablesFileTransfer`,
`enablesVehicleDataProtocol`, `enablesDCX`; the per-capability conclusions for those (§8 fileTransfer,
§9 uiSync, §10 DCX) are unaffected and still correct. Two further corrections that bear on this
document: `docs/carplay/05_METADATA_AND_CONTROLS.md` §2.1 records that the `Feature { start, stop, updates }` literals proposed in
§Mechanism B step (3) for `0xFFFA`/`0xFFFB` and `0xA100`/`0xA101` are **direction-inverted** and would
fail `declared_directions_match_apples_catalog`; and §1's "no `focustransfer()` lever exists" is
refuted — the lever and the `/info` emission both landed 2026-08-11, leaving only the SETUP
`enabledFeatures` echo (needed on BOTH the box path `session.rs:616-670` and the host-authored path
`VehicleConfig.swift:59-68`, or a box-only change is invisible under app-driven SETUP).

### R-53-2 · The serde gate now parses NINE keys, and "extend the serde gate" is no longer the unblocking change

- **Verdict:** CORRECTED
- **Landed:** `fix(doctrine): the app now owns uiAppearance, mapAppearance and focusTransfer` (`a35d743`, 2026-08-11)
- **Scope:** TL;DR bullet "One serde gate silently drops ~8 toggles" AND "Shared infrastructure" item 1 — the same text appeared verbatim at both sites, so both markers cite this one entry.
- **Note:** The three landings are `enablesLogTransfer` (2026-08-07), the `enablesMainBufferedAudio`/`appDrivenSetup` pair, and the docs/carplay/04_CAPABILITIES_AND_CONFIG.md #25 trio in `a35d743`.

**STATUS UPDATE 2026-08-16 — the serde-gate claim above was correct on 2026-08-02 and is now three
landings stale.** `AccessoryConfig` moved to `vehicle_config.rs:351-408` and parses **NINE** of the
app's sixteen keys: the three named plus `enablesLogTransfer` (2026-08-07), `enablesMainBufferedAudio`,
`appDrivenSetup`, and the docs/carplay/04_CAPABILITIES_AND_CONFIG.md #25 trio `enablesUIAppearance`/`enablesMapAppearance`/
`enablesFocusTransfer` (`a35d743`, 2026-08-11). All nine are armed per control connection at
`airplayd/src/main.rs:676-728`. **Still dropped (7):** `enablesVideoPlayback`, `enablesEnhancedSiri`,
`enablesUIContext`, `enablesUISync`, `enablesFileTransfer`, `enablesVehicleDataProtocol`,
`enablesDCX` — each names a capability with **no box implementation behind it**, so "extend the serde
gate" is no longer the unblocking change described here: the parse is a one-line formality and the
real work is per-capability `/info` emission plus the SETUP `enabledFeatures` echo. Add a field only
in the same commit that makes the box do something with it. Line refs `:232-249`/`:238-249` no longer
resolve.


## docs/carplay/02_SESSION_LIFECYCLE.md — App-driven SETUP: go/no-go (2026-08-09)

### R-55-1 · The default AND transport statements were overtaken — app-driven SETUP runs on both transports and defaults ON for wired

- **Verdict:** SUPERSEDED
- **Landed:** `feat(setup): make app-driven SETUP the default (wired)` (`ba1df2a`, 2026-08-09)
- **Scope:** The verdict line, the "Wired: GO behind the default-OFF toggle" passage, the "Remaining before app-driven becomes the default" list, and every "wireless stays box-driven / a separate later milestone" statement. The wireless half landed 2026-08-10 by owner directive (docs/carplay/04_CAPABILITIES_AND_CONFIG.md).
- **Note:** 273aed1 was backticked in the source. The backticks were stripped when moving (the hash resolves in no ref, and backticked SHAs must resolve per `tools/docs_status_check.py`); no character of the hash was changed. This is the only deviation from verbatim in this entry.

**UPDATE 2026-08-09/10 — the default AND transport statements in this doc were overtaken.** Every
"wireless stays box-driven / a separate later milestone" statement below (including the verdict at
the top, and the later-milestone framing throughout) is SUPERSEDED: the wireless flip landed
2026-08-10 by owner directive — app-driven SETUP now runs on BOTH transports, with the host
preserving the box-only `iAPChannel`/`sessionManagement` feature tokens (docs/carplay/04_CAPABILITIES_AND_CONFIG.md). By owner
directive the default flipped ON for WIRED in commit `ba1df2a` (2026-08-09; this line originally cited
273aed1, a hash that resolves in no ref — corrected 2026-08-16): `appDrivenSetup`
defaults true, the ≥1-week soak now runs in production, and box-driven SETUP is the selectable
sticky fallback rather than the default. Per docs/carplay/04_CAPABILITIES_AND_CONFIG.md, app-driven is the standing design default
project-wide. This supersedes the verdict line above, the "Wired: GO behind the default-OFF toggle"
passage, and the "Remaining before app-driven becomes the default" list; those passages stand
unedited as the go/no-go record as written.

### R-55-2 · Four commit hashes cited in docs/carplay/02_SESSION_LIFECYCLE.md resolve in no ref; the real ones are recorded

- **Verdict:** CORRECTED — hashes only
- **Landed:** `Docs QC: batch-fix the remaining audit findings across 51 files` (`a08b0eb`, 2026-08-16)
- **Scope:** "Validation results (P0–P3, hardware)" and the wired default flip in `R-55-1`
- **Note:** c5af75f, b09aee7, 3d56e28 and 273aed1 were backticked in the source. The backticks were stripped when moving, for the same reason as in `R-55-1`; no character of any hash was changed. The four replacements (`84d2b80`, `692cc80`, `89c457b`, `ba1df2a`) all resolve and stay backticked.

**CORRECTED 2026-08-16 — the four commit hashes originally cited in this doc resolve in no ref.** They
have been replaced with the real ones (`git log` subjects unchanged, dates unchanged). Old → new:
c5af75f → `84d2b80` (P0+P1), b09aee7 → `692cc80` (P2), 3d56e28 → `89c457b` (P3),
and 273aed1 → `ba1df2a` (the wired default flip, above).


## docs/carplay/04_CAPABILITIES_AND_CONFIG.md — App-Driven Doctrine (owner directives, 2026-08-10)

### R-56-1 · The 2026-08-10 corrections sweep checked doctrine only — "clean" does not mean "currently true"

- **Verdict:** CORRECTED — scope of the sweep
- **Landed:** `fix(wireless): stop refusing iOS's type-130 DataStream SETUP — the dead metadata plane` (`9a5e38e`, 2026-08-10)
- **Scope:** §4 "Corrections index (2026-08-10)" — the three doc lists (living / annotated / clean). The doctrine itself is NOT in question.

**⚠️ Scope correction, 2026-08-10 (same day).** This sweep checked docs for **doctrine** conflicts —
whether a doc claims box ownership of something that should be app-driven. It did NOT verify
**operational status** claims, and "clean" must not be read as "everything it asserts is currently
true". Same-day counter-example: docs/carplay/05_METADATA_AND_CONTROLS.md and the handoffs are listed clean here while asserting a
working wireless metadata plane that had been dead since 2026-07-31 (docs/carplay/05_METADATA_AND_CONTROLS.md §8), and docs/carplay/05_METADATA_AND_CONTROLS.md is
listed clean while saying "one defect remains". Those were corrected separately. A doctrine sweep is
not a status audit; the two need to be run as different passes with different questions.


## docs/wireless/01_BT_AND_RADIO.md — The radio HAL: making OCBM chipset-agnostic (2026-08-15)

### R-57-1 · `/etc/carplay_ident` is not yet the single source of truth — three other name derivations ignore it

- **Verdict:** CORRECTED
- **Landed:** `Docs QC: revert my own docs/wireless/01_BT_AND_RADIO.md "fix" - the original was right for the CCPA` (`acb880b`, 2026-08-16)
- **Scope:** §6c "One box, one name — the intent, and why it is NOT yet achieved"; the divergence it claimed to have eliminated. Open work is tracked in §7.
- **Note:** The moved text contains its own nested correction-history note, recording that an audit "corrected" the `hciconfig` line to raw-HCI and was reverted the same day. That nesting is preserved exactly — it is the record of a WRONG correction, and `acb880b` is the reversal.

**CORRECTED 2026-08-16 — the ident file is NOT yet the single source of truth, and "every later
caller reads that file" was wrong on the day it was written.** `radio_hal.sh` and `radio_ap_up.sh`
are its only readers. Three other derivations run beside them and none consults it:

* **`carplay-wireless` overwrites the advertised BT name every session.** `main.rs` computes
  `carplay_iap2_core::message::accessory_name(ACCESSORY_BRAND)` (`main.rs:88`, `ACCESSORY_BRAND =
  "CarLink"` at `:49`) and `bt_bringup::bring_up` (`main.rs:89`) writes it after its own DOWN→UP.
  **Which mechanism depends on the platform** *(corrected twice — see below)*: `bt_bringup` branches on
  `hci::native_selected()`, which is true only when `CARPLAY_HCI_BACKEND=native`. That variable is set
  by **`pi/tools/start_stack.sh` alone**, so the **Raspberry Pi** takes the raw-HCI path
  (`hci::write_local_name` + `hci::write_eir`) while the **CCPA takes the `hciconfig <dev> name` path**
  — the default, since the env is unset there.

  > *Correction history, kept because both errors are instructive: this line originally said
  > `hciconfig`, which is right for the CCPA. On 2026-08-16 an audit "corrected" it to raw-HCI on the
  > strength of reading `bt_bringup.rs:133-134` without the `native_selected()` branch above it — the
  > Pi-only path. It was corrected back the same day once `hci.rs:81-89` and `pi/tools/start_stack.sh`
  > were actually read. If you change this line again, check which branch the platform you mean takes.* The supervisor execs `carplay-wireless` *after*
  `radio_hal.sh bt_on` inside the same detached wrapper (`session_supervisor.sh`, `wireless_up`),
  so the last writer wins and the controller advertises `CarLink-<suffix>`, not the seam's
  `ccpa-<4hex>`. The seam's name is not wholly inert — `bt_set_name()`'s writes to
  `/etc/.custom_bluetooth_name` and `/etc/bluetooth_name` still govern the `bluetoothDaemon`
  path — but it is not what the phone sees on the mapped path.
* `bt_on.sh` (the owned IW416 path) derives its own `ccpa-<4hex>` from `set_wifi_mac`, then the
  serial.
* `ocbmd`'s `bt_name_from()` derives `CarLink-<4hex>` from the Wi-Fi MAC then the serial — and
  that is the name the app's CCPA tab reports, i.e. the one the owner actually sees.

The suffixes can diverge too, not just the `ccpa-`/`CarLink-` prefixes, because the two chains do
not share a source list. `box_name()` tries the ident file → the live WLAN interface's MAC → the
vendor `set_wifi_mac` helper → the **BT controller** address → `/etc/serial_number`;
`accessory_name()` tries `wlan0`'s MAC → `/etc/serial_number` → `/sys/devices/soc0/serial_number`.
In the BT-only bridge role no WLAN interface exists, so the seam falls through to `set_wifi_mac`
(or, on a unit where that helper was stripped, to the BT controller address — a source the Rust
chain never consults) while `accessory_name` falls straight to the serial: two suffixes for one
box. The divergence §6c claimed to have eliminated is therefore still live. Closing it means
having the Rust side read the ident file first; that is a behaviour change and is tracked in §7,
not claimed here.


## docs/SESSION_KICKOFF — Session Kickoff

### R-KICKOFF-1 · The "Current state" section is three weeks and five documents behind, and is not the current session opening

- **Verdict:** STALE
- **Landed:** `Docs QC: three more dead greps, and six status-ledger items that shipped` (`28f9fdd`, 2026-08-16)
- **Scope:** §"Current state (update when it changes — 2026-07-25)". The citations in it still resolve; the framing is what is out of date.

**⚠️ STALE 2026-08-16 — three weeks and five documents behind. Do not use this section to decide what
to work on.** The type-130 carrier it frames as work-to-do is implemented (`receiver::datastream`) and
already has a regression capture (`docs/ops/captures/2026-08-10_REGRESSION_datastream130_scid_rejected.txt`).
It also predates the current mandatory opening: per `README.md` and `../ops/04_OPEN_ITEMS.md`,
the FIRST ACTION is the handoff + CLAUDE.md + docs/carplay/04_CAPABILITIES_AND_CONFIG.md, not this file. Everything this section *cites*
still resolves — it is the framing that is out of date.
