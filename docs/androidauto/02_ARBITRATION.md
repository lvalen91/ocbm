# Projection arbitration — CarPlay vs Android Auto

> **STATUS:** CURRENT · single owner for this topic. Split out of `docs/host/02_ANDROID_AUTO.md` on 2026-08-31 when Android Auto got its own category. Correct this file in place — do not add a sibling.

**Contents:** what the box does to detect and arbitrate between CarPlay and Android Auto — iPhone vs
Android, wired vs wireless — so the right path starts and a live session is never interrupted by
another connecting phone.

Originally a design note grounded in a 3-agent code audit plus live headless probing of the box
(NCM/SSH, box in NCM mode). **It is no longer a plan — §4 and §5 describe shipped, device-tested
behaviour.** §2 is kept as the pre-fix baseline because §4 and §5 are only legible against it.

### 0. The model, stated by the owner (2026-09-01)

Everything below is an implementation of this one rule, and it is worth stating before the detail
because sessions keep re-deriving it from the code and getting it subtly wrong:

> **The adapter is first-come, first-served.** It idles advertising and listening for *all* of it —
> wireless and wired, iPhone and Android. **The first connection to arrive owns the session, and
> that protocol is the active one for its duration. Every other connection is rejected until it
> ends.** The user can enable or disable wireless mode to force wired-only.

Three consequences that answer questions the code does not make obvious:

- **A single mutual-exclusion slot is CORRECT, not a limitation.** `control::SessionClaim` is one
  `AtomicBool` behind a `compare_exchange`. A reviewer reasonably asked whether Android Auto holding
  that slot should be allowed to block a CarPlay reconnect. Under this rule it should: that is
  first-come-wins working, not a bug to design around.
- **There is no preemption, in either direction.** CarPlay does not outrank Android Auto and Android
  Auto does not outrank CarPlay. Whoever arrived first keeps the box. This is the same doctrine the
  wired path already follows and the reason Hot-Handover is opt-in and off by default.
- **`wireless: false` means wired-only for BOTH protocols.** It is the user-facing switch named in
  the rule above, and it is why `wireless_up()` gating radio bring-up on the CarPlay `wireless:` key
  is correct as written. Wireless Android Auto rides the radios that flag raises and deliberately
  gets no separate switch — a proposal to loosen that gate so AA could raise radios on its own was
  dropped once this rule was stated, which also avoids editing `wireless_up()`'s documented
  load-bearing choke point.

What this rule does NOT cover, because it happens before any session exists: **SDP browsing.** A
phone browses while the box is idle, i.e. before anyone has claimed anything, so first-come-wins
does not arbitrate it. Two phones in range can contend for the SDP server, and today it serves
exactly one client at a time — see §6.

### 1. Current reality (what exists)

**Arbitration hub = `tools/session_supervisor.sh`** (installed to `/script/`), driven by `/tmp` flags
written by `ocbmd`:
- `host_present` — master app-session gate (ocbmd, on CT_SUBSCRIBE). Box IDLE until an app connects.
- `carplay_transport` = `wireless` when the wireless arm owns the session (`av.rs:274`); absent = wired-or-idle.
- `phone_present`, `radio_off` (app CT_RADIO), `carplay_cfg.yaml` (app-pushed YAML config).

**CarPlay wired↔wireless arbitration is robust** (all in session_supervisor):
- `wireless_owns_session()` → first-come-wins; wired `arm()` refuses while wireless owns.
- Hot-Handover **default OFF** → a fresh wired plug does NOT preempt a live wireless session
  (matches Apple WWDC 2017-717 "do not interrupt a running session"). Opt-in `hot_handover: true`.
- Radio-yield mirror parks the wireless radio (hostapd/hci0) for the duration of a wired session.

**Doctrine (docs/carplay/04_CAPABILITIES_AND_CONFIG.md): app-driven.** Box carries no opinions; the app pushes config (wireless on/off,
hot-handover, pairing mode, WiFi creds) via CT_SUBSCRIBE → `carplay_cfg.yaml`. There is **no** host
opcode to explicitly pick wired-CP / wireless-CP / AA (`CT_MODE_SELECT` is unrelated — projection vs
console debug). The Rust `/run/carplay/arbiter.sock` is a STUB (always grants standalone); all real
arbitration is the shell supervisor.

**Phone-type detection was two ISOLATED probes** — CarPlay grepping Apple `05ac`, `aa-bridge`
matching Google `0x18d1`, neither aware of the other vendor. It is now one resolver:
`box_common::phone::classify` / `classify_dev`, shared by both paths, with hubs excluded by
`bDeviceClass == 0x09` (see §5, F6).

### 2. Configuration matrix — the state this design started from

**This table is the PRE-FIX baseline, kept because §4 and §5 are only legible against it.** Rows 2,
5 and 6 are closed: AA now auto-selects, CP↔AA are mutually exclusive under one owner flag, and the
`phone_reset.sh` hazard is guarded. Read §4 for what happens today.

| # | Scenario | Behavior BEFORE the fixes in §4/§5 | OK then? | Gap |
|---|----------|----------------|-----|-----|
| 1 | Wired iPhone + app subscribed | CarPlay projects (projection_up→iap2d→airplayd) | ✅ | — |
| 2 | Wired Android + app subscribed | CarPlay no-ops (no 05ac); **AA never auto-starts**; phone just charges | ❌ | No auto-AA selection |
| 3 | aa-bridge run while iPhone present (normal) | `find_phone` 18d1-guard skips iPhone; **zero control transfers reach it** | ✅ | — |
| 4 | iPhone mid-CarPlay, aa-bridge started | iPhone role-switched to host → invisible to aa-bridge's host bus | ✅ | — |
| 5 | Android mid-AA, app subscribes (projection_up runs) | grep 05ac no-match → exits 1; `phone_waiting` latches; AA undisturbed | ⚠️ | Latent `phone_reset.sh` hazard (see §3) |
| 6 | Wireless CarPlay active + wired AA phone plugged | **No guard.** If aa-bridge runs it grabs ci_hdrc.0 unmediated → collision | ❌ | No CP↔AA arbitration |
| 7 | Wired CarPlay active + phone in BT range | Wireless defers (Hot-Handover off); radio parked | ✅ | CP-only (AA has no wireless) |
| 8 | Wireless Android Auto | Does not exist | — | Phase 3 — bootstrap + pump now built (§4, [`03_WIRELESS.md`](03_WIRELESS.md) §6b/§6c); not device-tested |

Incidental safety even then: strict per-VID self-filtering (CP=05ac, AA=18d1) + role-switched devices
becoming invisible to the other path prevented cross-wiring in 1,3,4,5. The real holes were **2** (AA
never selected) and **6** (no CP↔AA mutual exclusion), plus the latent **5** hazard — all three closed
by §4 and §5.

### 3. The latent hazard — CLOSED

`session_supervisor`'s `PROJ_AT=3` failure ladder → `escalate()` → `phone_reset.sh` does a **real USB
port reset of ci_hdrc.0**. It was capped at 1 failure by the `phone_waiting` debounce, so it was never
reached — but only the debounce stood between a live AA session and a port reset. `escalate()` now
returns early while AA owns the session (`session_supervisor.sh`, `aa_owns_session` guard), mirroring
the existing `wireless_owns_session()` guard. `kill_session()` and `arm()` carry the same check, and
the `phone_reset.sh` call site sits inside `escalate()`, so it is covered transitively.

### 4. How arbitration works today

Single-owner flag in `box-common`, one definition of the token spellings shared by ocbmd, aa-bridge
and the shell (`box_common::flags::owner()`).

- **CarPlay claims it.** `arm()` calls `claim_carplay_owner()`; `kill_session()` releases. A `wired-cp`
  claim with no `airplayd`/`iap2d` alive is stale, cleared, and falls through, so a crashed CarPlay
  session cannot lock AA out permanently.
- **AA claims it before it can serve.** `aa-bridge` prepares the accessory and claims the flag
  **before `accept()`**, serves one session per prepared accessory, clears at session end, and loops.
  This was forced by a real bootstrap deadlock: the bridge used to claim inside `serve_session`, i.e.
  only after a host connected, while the app only connects after seeing `PM_WIRED_AA` — each side
  waited for the other and the box sat idle with a running bridge and a plugged-in phone. Claiming
  before `accept()` also keeps the flag honest: `wired-aa` means a live accessory link exists. Every
  wait on that path is bounded, since claiming early makes a hang a lock-out.
- **Wireless AA claims it in two hops, and the second one ADOPTS the first.** `carplay-wireless`
  claims `wireless-aa` when the Bluetooth bootstrap establishes and deliberately HOLDS it across the
  Wi-Fi association (`run_aa_bootstrap`), so nothing takes the box while the phone is mid-handoff.
  That bootstrap runs from the channel-4 ACCEPT path — the phone dials us, which is the direction
  gearhead actually uses ([`03_WIRELESS.md`](03_WIRELESS.md) §2f). What the box does beforehand is
  raise a headset link (`reconnect::attempt_headset`), because the phone will not dial until its own
  `BluetoothProfile.HEADSET` reports us connected (§6b). **That headset link takes no claim at all**
  — it is not a projection session, and claiming for it would lock CarPlay out of a box that is
  merely holding a Bluetooth link open. It does stand down before dialling if ANY owner is set, and
  releases immediately if another projection claims the box while it waits: first-come-wins, §0.
  (A 2026-09-04 intermediate version had `reconnect::attempt_aa` dialling the PHONE for the AA
  record and taking the claim itself. The phone hosts no such record; that code is deleted.)
  When the phone then dials the AP endpoint, `aa-bridge`'s wireless arm finds the flag already set
  and adopts it rather than re-writing it (`pump::decide_wireless_claim`: idle → take, `wireless-aa`
  → adopt, anything else — including `wired-aa` — → refuse and close). Released at session end by
  whichever of the two gets there first.
  **Consequence worth knowing:** the two processes write the SAME token, so "release only if it is
  ours" cannot tell them apart. `carplay-wireless`'s teardown can clear the flag under a live pump.
  Bounded — that teardown means the radio is going, so the session is over anyway — and fixing it
  means putting a pid in a flag file that three daemons and the shell parse. See
  [`03_WIRELESS.md`](03_WIRELESS.md) §6c.
- **The two AA transports arbitrate in-process as well as through the flag**, because the flag
  cannot express ordering. Both arms live in one `aa-bridge` process and both need the app's single
  `:5277` relay; a shared file gives each of them a read-then-write TOCTOU, and the app's connection
  would go to whichever polled first. So the wireless arm registers an in-process intent BEFORE it
  writes the flag, and the wired arm consults that intent both when it takes a client and at the
  point it would claim. The cross-process race (bridge vs `carplay-wireless` vs this supervisor)
  remains and is bounded, not eliminated: a wired arm that wins the flag race still cannot obtain a
  client, so it releases after its 30 s announce window instead of projecting.
- **`projection_owner()`'s `wireless-aa` liveness probe stays `pgrep carplay-wireless`** and is
  deliberately NOT widened to also accept `aa-bridge`. With `--wireless` the bridge is resident, so
  `pgrep aa-bridge` is true whenever the wireless stack is up — accepting it would make the
  stale-flag self-heal unreachable, which is strictly worse than the case it would cover.
- **The box tells the app which mode is live.** `CT_PROJ_MODE = 0x19`, payload `[CT_PROJ_MODE][PM_*]`
  with `PM_NONE 0x00 / PM_WIRED_CP 0x01 / PM_WIRELESS_CP 0x02 / PM_WIRED_AA 0x03 / PM_WIRELESS_AA 0x04`
  (all five now reachable — `PM_WIRELESS_AA` since 2026-09-04). `ocbmd` needed no change for it:
  `proj_mode_tick` is `owner().wire_code()` and `handle_ip` connects to whatever target the host
  names, so the wireless transport rides the existing CH_IP relay to the same `127.0.0.1:5277`.
  `ocbmd::proj_mode_tick` emits on change only,
  throttled to ~2/s once latched, re-armed on every fresh `CT_SUBSCRIBE` so a re-attaching app learns
  the current mode immediately. Same discipline as `CT_BT_PHASE`. Additive per the OCBM extensibility
  rules — frozen envelope, no version bump. Plugging an Android phone into a box with a subscribed app
  therefore projects AA with no env var and no user action.

### 5. Failure modes closed, and why each mattered

- **CarPlay hijack.** An already-resident `aa-bridge` could claim the box during a live wired CarPlay
  session; the app would park the CarPlay decoder and switch to AA mid-drive while the supervisor
  treated the still-running CarPlay stack as handed off. Neither existing stand-down covered it —
  `carplay_session_live()` gates only bridge LAUNCH, and `apple_on_bus()` goes blind the moment the
  iPhone role-switches out of `05ac`. The bridge now stands down on `wired-cp` at every point it could
  claim: loop top, immediately before `set_owner` (CarPlay can arm during the multi-second AOAP
  switch), on the unclaimed→client path, and inside the wait loop so a parked bridge notices within
  250 ms. `release_owner_if_ours()` replaced every blanket `clear_owner()` — the bridge had been
  deleting CarPlay's claim while cleaning up after itself, found by device test, not by review.
- **F6 — a bare hub is not a phone.** `phone::classify()` treated every non-Apple, non-root-hub VID as
  an Android candidate, so a hub, dashcam or card reader launched a permanently resident bridge that
  AOAP-probed it forever (the precondition for the hijack above) and suppressed wireless CarPlay.
  Hubs are now excluded by `bDeviceClass == 0x09`, mandatory in every hub device descriptor (USB 2.0
  §11.23.1) and never used by a phone — Apple, Android normal mode and AOAP accessories are all
  per-interface `0x00`. Measured on the box: Pixel `vid=18d1 class=00`, root hub `vid=1d6b class=09`.
  `apple_on_bus()` stays deliberately VID-only: over-inclusive there fails toward CarPlay.
- **F4 — a crashed app wedged AA.** ocbmd's `CT_HELLO` reattach cleared the output queues but not
  `self.conns`, so a host that died without `CT_STOP` and relaunched inside the heartbeat grace left
  its corpse's CH_IP socket being pumped while the new host's `IP_OPEN` sat unaccepted in the bridge's
  backlog.
- **F3 — the Android Auto toggle did not reach a running bridge.** `android_auto: false` was read in
  exactly one place, `session_supervisor.sh::aa_enabled()`, guarding `arm_aa` — the launch. Nothing
  consulted it afterwards, so turning AA off during a live session did nothing until the phone was
  unplugged. It is now checked inside the session loop as well.
- **F2 and F5** were settled on hardware 2026-08-27 and are closed.
- **Race: `wireless_up` vs AA ownership** — fixed 2026-08-24.

