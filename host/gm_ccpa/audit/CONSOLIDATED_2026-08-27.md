# Consolidated audit — 20 independent Fable agents, 2026-08-27

Two repos: `gm_ccpa` (Android app + JNI), `ccpa_custom` (box firmware + vendored Rust).
Every finding below carries an agent's code citation; log-backed ones carry file+timestamp.
"UNVERIFIED" = source-only, no hardware evidence. Hardware was disconnected for most of the audit.

> **ADJUDICATED 2026-08-27 by six adversarial reviewers.** The severities and claims in the
> tier sections below are the *first-pass* record. Where the review section at the end of this
> document ("Adversarial review verdicts") differs, **the review wins** — it is the authoritative
> severity and the only text cleared for planning. Several first-pass findings were refuted
> outright; do not act on a tier entry without checking its verdict.

## Tier 0 — Security

**0.1 CRITICAL — accessory private key is a public constant.**
`CarPlayRx.kt:84` `edSeed = ByteArray(32){ (it*31+7).toByte() }` is the Ed25519 long-term
private key (→ `Identity::new` → `SigningKey::from_bytes`). Pair-verify's whole authenticity
guarantee is the signature at `verify.rs:132`. Anyone can recompute it, stand up a rogue
receiver on the same SoftAP, and be authenticated by an already-paired iPhone — no setup code.
Also signs pair-setup M6. Key-compromise-impersonation. Tracked as deferred T6.5, which
undersells it: it is the only finding with a remote attacker. Fix = SecureRandom seed persisted
beside the peer store; invalidates all pairings; must ship alone.

**0.2 HIGH — peer store accepts hostile (id, LTPK) with no validation on load**; pair-verify then
treats it as a paired controller. JNI writer uses a fixed `bin.tmp` (box side uses unique temps),
reintroducing a concurrent-write tear.

**0.3 HIGH — PII survives log export.** `SessionSummary` defers redaction to the export redactor;
the redactor matches shapes, not key names, so box **serial** and both **device names** (incl. the
user-assigned phone name from `CT_PHONE_IDENT`) export verbatim. `RE_WPA` omits `pass` — the literal
name of the run-extra carrying the hotspot passphrase. Mirror defect: `RE_LONG_DIGITS` destroys
`start_epoch_ms` in every exported SESSION line.

## Tier 1 — Crash / memory safety

**1.1 CRITICAL — VoiceRouter sweeper releases a live codec.** `sweepIdle()` on `cp-voice-sweep`
calls `Sink.release()` (`MediaCodec.stop()/release()`) while `route()` calls `sink.feed(au)`
OUTSIDE the `sinks` lock on the :9003 thread. Unmaps direct ByteBuffers under a feeding thread —
SIGSEGV class. `lastAudioAt` advances only on energetic PCM, so a sink idles out *while AUs arrive*
(digital silence). Sweeps observed live: `[voice] call: idle 3s — released`. Comment at
`VoiceRouter.kt:254` asserts consume-thread-only ownership; the sweep was moved off that thread and
the invariant broke silently.

**1.2 HIGH — panic can unwind across the JNI boundary → SIGABRT.** `lib.rs:522`
`byte_array_from_slice` sits outside `catch_unwind`, as do `eprintln!` in the catch handlers.
`eprintln!` panics if stderr fails; stderr is the logcat pipe; since Rust 1.81 unwinding out of
`extern "system"` aborts. Trigger: drain-thread death → EPIPE. The file's own comment (`:74-78`)
worries about that scenario. UNVERIFIED on hardware.

**1.3 HIGH — `std::env::set_var` in `JNI_OnLoad` is a data race (UB).** ART threads run before
`JNI_OnLoad`, and the drain thread is spawned *before* the `set_var` calls. Compiles silently only
because the crate is edition 2021. Fix: reorder, or use the existing programmatic `levers::set_*`.

**1.4 MED — `unsafe impl Send for Native` is sound today but wrongly justified.** The SAFETY comment
argues from a mutex, which justifies Sync-like access, not Send. Field-by-field audit passes for the
current types; the blanket impl silences the checker for any future `SessionDelegate`. Fix:
`Box<dyn SessionDelegate + Send>` and delete the impl.

**1.5 MED — `surfaceDestroyed` returns without joining the render thread**, contrary to the Android
contract. Degrades to a caught exception usually; vendor OMX BufferQueue races are where SIGSEGVs
live, and this unit runs `OMX.Intel.hw_vd.h265`.

**1.6 HIGH — `wireless/src/mfi_local.rs` accepts a partial I2C transfer.** It checks `>= 0` where its
claimed byte-for-byte twin `mfi-i2c-local` checks `== 2`/`== 1`. `I2C_RDWR` may legally return a
positive partial count; a partial on the final signature read returns an **all-zero buffer as a valid
RSA signature**. Fix is two constants.

## Tier 2 — Brick / recovery paths

**2.1 HIGH — dead-man can consume itself and leave the box unreachable.** `ocbm_boot.sh:77-79`:
`touch /script/ncm_only || echo WARNING; rm -f /script/ocbm_trial; sync; reboot`. If the touch fails
(jffs2 full — `/script/*.log` accumulate there, ~4 MB free) the trial flag is removed and the box
boots into OCBM with no dead-man and no NCM: unreachable without UART. Failover watchdog has the
mirror bug (boot→fail→reboot loop). This is the exact mechanism relied on when flashing `ocbmd`.

**2.2 HIGH — L2 escalation is broken three ways.** `pkill -f ocbmd` also kills the inittab wrapper;
the wrapper watches `/usr/sbin/ocbmd` and is blind to L2's bare-name `setsid ocbmd`, so it starts a
**second** daemon ~5 s later; the success check `pgrep -f ocbmd` matches the wrapper, so "L2: ocbmd
restarted" logs even when ocbmd is dead. Two writers on `/tmp/host_present` manufacture flaps → L3
reboot.

**2.3 MED — `pkill -f` overmatch in `kill_session`/`escalate`/`preempt_wireless_for_wired`.** The
detached `wireless_up` wrapper's `sh -c` body contains `airplayd`/`rx-connect` literals, so plugging
a phone during the ~15 s bring-up kills it mid-`radio_hal`: radios half-up, `btd` never
exec'd, no error surfaced. `wireless_down` already uses bracketed `[a]irplayd` forms; the fix was
never applied elsewhere.

## Tier 3 — The dial-back stall (root-cause chain)

**3.1 ROOT CAUSE — `carManager_handlePendingAutoconnect: No matching endpoint found for deviceID
B2:D6:2A:9F:C9:30`** (iPhone unified log, every stalled nudge). `200 OK` means only "pending
autoconnect set". airplayd needs an endpoint object, which requires a **TXT** record; in stalls iOS
holds only the PTR (TTL 4500) while TXT/SRV (TTL 120) have expired and cannot be refetched. The
TXT-vs-SRV signature is a *symptom*: SRV is queried only after TXT exists.

**3.2 — `MulticastSocket.joinGroup(InetAddress)` fails ENODEV.** Single-arg form makes the kernel
route-lookup 224.0.0.251; the head unit has no multicast route in any policy table
(`ip route get 224.0.0.251` → "Network is unreachable"). Two-arg `joinGroup(InetSocketAddress, NIF)`
passes ifindex and skips the lookup. Affects `MdnsResponder.kt:79` and `MdnsInspect.kt`.
**CORRECTED onset:** the 16:01:23 ENODEV lines are the *live-lookup* socket. The **responder** first
failed at **16:26:58.785**; the underlying breakage began between 15:37:49 and 16:01:23 (candidate
trigger 15:40:27 `LegacyTypeTracker.remove` + `iptables -D tetherctrl_raw_PREROUTING -i br0`).
Already-bound sockets kept working; only new binds fail.

**3.3 — `/tmp/bt_phase` is never idled.** Three `publish_bt_phase` sites, all forward-progress;
`phase_for()` has no `BTP_IDLE` arm; `BTP_IDLE` appears repo-wide only at its definition; nothing
unlinks the file. It means "deepest phase since boot". `ocbmd` re-emits it to every fresh subscriber.
Observed false "PHONE DETECTED" windows: 47 s, 78 s, 133 s, 5 m 47 s. Confirmed independently by
three agents (log-first, source-first, daemon-side). Daemon-side agent narrowed the blast radius:
of the six re-emitted values, **`bt_phase` is the only liar** — `phone_state`/`proj_mode` read live
flags, `pairing_code`/`phone_ident` were observed correctly cleared, `box_health` is sampled live.
Fix: publish `BTP_IDLE` at `bt_driver.rs`'s exit funnel, in `run_active_session` teardown and at
process start; `rm -f /tmp/bt_phase` in `wireless_down`. Do NOT add an Idle arm to `phase_for`.

**3.4 — stale `SEV_HOST_GONE` is the first frame the next host reads.** On stop-grace expiry,
`go_idle()` → `set_present(false)` emits `HOST_GONE` toward a host already gone; the frame sits in
the gadget FIFO and `CT_HELLO`'s queue-clear cannot retract it. Observed 17:27:27 and 17:33:02.
The app logs "box dropped us" and drops to idle on a link that just came up.

**3.5 — envelope replay flag (proposed).** `flags` bit2 = `F_REPLAY`, set by ocbmd on frames emitted
by re-subscription rather than by change. Bit2 already documented reserved; no receiver validates
flags either end; degrades cleanly both directions; zero payload change. Fixes the whole latched
class at once.

## Tier 4 — Defects in changes made 2026-08-27 (mine)

**4.1 — `armHandoffWatchdog` calls `escalate()` with no `to(...)`.** I fixed exactly this in the
grace and inbound timers and missed this one. Confirmed live 17:28:14: phase stranded in
`HANDOFF_SENT` with zero timers armed.

**4.2 — ladder cooldown skip is INVERTED.** A cooling cheap rung *promotes* to a more expensive one.
Observed 17:31:34: `MGMT_RESTART_WIRELESS` as first response, dropping a pairing in progress. With
all rungs cooling it declares STALLED in the same millisecond, having taken no action. Also
`lastRungAt` is stamped before `runRung`, so a skipped dead-box rung burns its cooldown.

**4.3 — rung 1 self-triggers rung 2.** `CT_RADIO`'s off-edge makes box health regress → re-escalate;
the on-edge makes it green → `cancelRecovery()` resets `rung`. Observed 49 s with no timer armed.

**4.4 — `CT_HELLO` host label consumes the protocol's only host→box extension point.** Defined as
"everything after the nonce", undelimited UTF-8. Once shipped, no field can ever be appended. Repeats
the documented `0x13` caps-era mistake. **Still uncommitted — fix now** as TLV `[type][len][val]`.
(RFC 6709 §4.2.)

**4.5 — MFi correlation tag exists only as an ocbmd comment.** Absent from `ocbm-proto` and
`docs/02` (which still specifies the 3-byte header). A third implementation would silently break.
Rests on previously undocumented must-ignore leniency (RFC 9413 §5).

**4.6 — tag-match path skips the payload sanity check.** A corrupted `rlen` plus a 1-in-256 byte
coincidence returns a truncated body as OK. One line.

**4.7 — `BH_SSP` once-per-session cache samples during radio bring-up.** First tick fires ≤2 s after
SUBSCRIBE, while the supervisor is cycling radios; `hci0` is down, false is cached for the session.
The SSP bit is clear in every captured sample. Also stale across `MGMT_RESTART_WIRELESS`/`CT_RADIO`.

**4.8 — my deadline fix was half a fix.** Tunnel got 4 s (correctly wired, empirically sufficient:
cert ~1.1 s, sign ~1.7 s). The control path I deliberately left alone runs **12 s cert / 15 s sign**
against the phone's **10 s** request timeout.

**4.9 — 4 s alone is insufficient.** A cert/sign failure yields `NoCommit`; the tunnel link-ACKs
before executing and Zero-Ack has no retransmit, so the phone never re-asks. One transient burns a
full 120 s budget; three exhaust the session. Retry must live in `tick()`.

**4.10 — after give-up the tunnel cannot restart.** It awaits a `modesChanged` nudge that is
one-shot per session and already consumed. Zero tunnel lines after give-up, confirmed.

**4.11 — mutex poisoning is silent and permanent, and reachable** (`carplay-jni` is `panic=unwind`,
contradicting `plock`'s "production is panic=abort" rationale). `set_remote_signer` no-ops silently;
`remote_signer()` maps poison to `None` → "no signer installed", a lie. One caught panic disables
every future tunnel handshake for the process.

**4.12 — first re-subscribe after `HOST_GONE` is immediate**, and the 2/4/8/15 s ramp keeps the first
four cycles inside the ~20 s flap window that reboots the box. Never executed in any capture.

**4.13 — `rearm_presence_silently` manufactures flap edges.** The supervisor cannot distinguish
synthetic re-arms from real ones; rapid settings toggling can reach 5-edges/20 s and fire a USB
`phone_reset` at an innocent phone, then ratchet toward reboots.

## Tier 5 — Correctness, observability, protocol hygiene

**5.1** `SessionSummary` `frames`/`no_video` are false on every non-clean exit — `framesRendered` is
populated only by `HevcRenderer.stop()`, which does not run on `host_gone`/supersede/shutdown.
Proof: `ttff_ms=292075 frames=0 no_video=true`. **This invalidates evidence I previously relayed.**
`ttff_ms` also measures subscribe→frame, not connect→frame.

**5.2** Log `sweep()` applies the running session's budget across every `netprobe-*.log`, so an
OWN_PROCESS session (64 MB) prunes ~700 MB of whole-OS capture. The same pid alternated scopes four
times today, during evidence collection.

**5.3** Counters report success while dropping: audio kernel-side UDP overflow happens before
`recv_from` so neither `frames` nor `drops` moves; `AacPlayer` counts frames with `track == null`;
`VoiceRouter` counts size-dropped AUs; no `getUnderrunCount()` sampling.

**5.4** AudioFocus loss ignored everywhere — no listener on the media request, `REQUEST_FAILED`
discarded, live `focus change -2` unhandled. Either GM ducks us silently or we play over the tuner.

**5.5** `AvSink.stop()` closes listeners but not accepted sockets → parked producer connection,
silent no-audio on a healthy-looking session.

**5.6** Multi-touch: after the primary finger lifts, the second finger's MOVEs are sent *after* the
UP with no DOWN, then a second UP. `ACTION_CANCEL` sends a synthetic UP (cancel becomes a tap).

**5.7** BT driver SYN retransmit uncapped (~120 vs the documented ≤10; the 11th triggers
`NotifyConnectionFail`). The tunnel honours the cap; the BT path does not.

**5.8** Credential-less subscribe leaves `/etc/hostapd.conf` untouched — jffs2-persistent — so
`0x5703` serves the **previous vehicle's** SSID and PSK.

**5.9** NsdManager registration failure permanently disables dialling: `advertisedAt` is set only in
`onServiceRegistered`, and `connectOutWithRetry` spins on it at 4 Hz forever, one thread per
rediscover cycle.

**5.10** `server` is a plain `var` written on a pool thread → confirmed false `listening: FAIL` in
`selfTest`, plus a JMM race where `stop()` may read null and leak the bound port.

**5.11** mDNS: no probing (RFC 6762 §8.1) while setting cache-flush; AAAA answered with an A record
instead of NSEC (§6.1); announce spacing 250 ms vs §8.3's 1 s; three-way 5353 bind can black-hole
unicast-assist queries.

**5.12** `/info` ↔ feature-echo coupling in the GM app is comment-only (the box side regenerates per
connection). The documented `-16720` desync class can recur on any asset regen.

**5.13** Spec drift in `docs/02`: `CT_PROJ_MODE` implemented and live but entirely absent; `CT_*`
range still documented `0x01`–`0x18` (stale twice); CTRL table markdown broken, `0x19` row missing;
`CT_SETTIME` ack documented but absent from `lib.rs`.

**5.14** CH_FILE push uncapped on a ~4 MB rootfs; `.ocbm.part` temps orphaned by crash are never
swept. `forget_one_bond` rewrites the bond store without `sync_all`/dir fsync — a power cut can
resurrect a forgotten device.

## Corrections to the record

- **"Responder ENODEV since 16:01"** — REFUTED as stated. Those were live-lookup sockets. Responder
  died 16:26:58. Underlying breakage started earlier (15:37:49–16:01:23).
- **"One OCBM bring-up per process is absolute"** — REFINED. pid 13286 claimed twice and
  `/auth-setup` still succeeded, because the first link was already dead. The doc's rule is
  over-broad; the real invariant is "one live probe instance whose relay the receiver holds".
- **"BT reconnect cures the stall"** — over-read from runs where the responder was always up. In
  F5, three full BT cycles produced no SRV. Provisional (2 stall windows).
- **The stalls are not one phenomenon** — F1 (no BT at all), F3 (BT identified, phone never on
  Wi-Fi), F2/F5/F6 (phone on Wi-Fi, TXT-only, inert 200s). Lumping them hides F1/F3.
- **`AV_DISABLED` "points at /bin/true"** — stale. It was tried and reverted: `wait_visible` matches
  full-path argv[0], so a vanishing binary caused a ~12 s stall under `AV_LOCK` per `0x5702` retry
  inside the bt_driver read loop. Current state is `AV_SUPPRESS=''`. Correct fix is an env consumed
  inside `av.rs`.
- **FGS "torn down in exactly the case it exists for"** — imprecise. Plain backgrounding stops the
  Activity (`surfaceDestroyed`, no `onDestroy`), FGS survives. It is lost on real destruction, by
  which point `stopSession()` has torn everything down anyway. Real defect: the session lives in the
  Activity.
- **`frames=0 no_video=true` as evidence of an absent phone** — unreliable per 5.1. `btp_max` stands.

## Baselines (from three instrumented successes)

Pair-verify after INBOUND 0.08–0.55 s · INBOUND→first frame 2.9–3.9 s (n=4) · WIFI_HANDOFF→INBOUND
5–9 s · `/feedback` 2.02 s · audio 47.6 frames/s continuous · video 21–24 fps active, 1.2–1.7 fps
idle, ~207 kbps avg · **0 AUs dropped in 15,531 frames** · a healthy session survives box USB death
(S3 streamed 3 min past it).

## Cross-cutting observation

The recurring failure mode is not sloppiness — it is **comments asserting invariants the adjacent
code violates**. `CT_BT_PHASE` "must never gate the LinkState machine" sits directly above the line
that gates it. `VoiceRouter`'s "only the consume thread may release" sits above a sweeper that
releases. "One bring-up per process" is documented and only instance-enforced. `SessionSummary`
promises a redactor behaviour the redactor does not implement. `mfi_local.rs` claims to be a
byte-for-byte port and has diverged. The comments are unusually good, which is exactly why they are
trusted and why the drift is expensive.

---

# Adversarial review verdicts (6 reviewers, 2026-08-27)

Each reviewer's default position was that its assigned findings were wrong. Verdicts below are
authoritative and supersede the tier sections above. Identifiers are placeholdered per policy:
`<PHONE_BT_ID>`, `<PHONE_WIFI_ID>`, `<BOX_NCM_IP>`, `<AP_SSID>`.

## Refuted outright — do not plan work against these

| # | First-pass claim | Why it fails |
|---|---|---|
| 1.2 | Drain thread can die on EPIPE, wedging the pump | Drain thread cannot die; the EPIPE path is unreachable. |
| 1.4 | Missing `Send` bound needs an `unsafe impl` | `pub trait SessionDelegate: Send` supertrait already exists — proven by deleting the `unsafe impl` and compiling clean. |
| 4.12 | Escalation ramp can trip the flap detector into a box reboot | `FLAP_N=5`/`FLAP_WINDOW=20 s`; ramp yields ≤4 attempts in 20 s, and an edge needs `HEARTBEAT_GRACE=10 s` so edges are ≥10 s apart (2–3 max). Flap goes to L1 `phone_reset` first (`L1_MAX=2`, `L2_MAX=2`) — a reboot is 5+ escalations away. |
| 4.7 (part) | SSP bit clear in every captured sample | `run_hu_172224.log` 17:28:20 shows `BOX_HEALTH 0x53 [HCI|SSP|…]`. Cache bug is real; nothing gates on SSP, so it is diagnostic noise only. |
| 4.4 (part) | The reserved field is the only extension point | False — a version byte exists. The `0x13` precedent cited alongside it is real. |

**Also corrected:** two reviewers reported the app absent from the head unit. They queried
`zeno.gmccpa`, the source namespace. The install ID is the deliberate fixed-handler squat
`android.car.usb.handler` — `dumpsys package` confirms `versionName=4.0`,
`lastUpdateTime=2026-08-27 16:29:38`. The app is installed and current.

## Severity changes

- **0.1 hardcoded `edSeed` — CRITICAL → HIGH.** Exploitation needs SoftAP access *and* MFi
  hardware, and yields receiver impersonation only, not controller.
- **0.2 → MEDIUM. 0.3 → MEDIUM but understated** — `wifi_pass:` is a second unredacted PSK sink.
- **1.6 → MEDIUM.** The i.MX driver is all-or-nothing and never returns a positive partial.
- **1.1 → MEDIUM** (was CRITICAL). MediaCodec throws ISE, which is caught; native-crash dropbox empty.
- **1.3 → LOW** (was HIGH), **and the reorder fix proposed for it was wrong.**
- **4.1 → LOW-MED**, not stranding: `escalate()` does arm `retryTimer`.
- **4.3 → UNDERSTATED.** The dead window was **unbounded**; 48.5 s is only when the phone
  rescued it of its own accord.
- **4.8 → the 10 s phone timeout is unsourced.** It appears only in comments written
  2026-08-27 in `NativeCore.kt` / `OcbmClient.kt` and in the audit that cites them. No capture,
  no doc. Nearest real numbers: CINEMO CT5's 10 s×3 (different layer) and a
  disassembly-confirmed CarKit `timeoutInterval = 30 s`. The budget asymmetry is genuine; the
  urgency attached to it was not evidence-based.
- **5.2 → keep the code defect, drop the loss claim.** Scope alternation on one pid is real
  (pid 31636 flipped four times), and an OWN_PROCESS sweep would prune whole-OS files to the
  64 MB ceiling. But max `bytesOnDisk` in the corpus is 7.69 MB and there is not one
  `ceiling: dropped` / `retention: dropped` / `disk critical` line anywhere. Nothing was lost.
- **5.4 → raised to confirmed.** GM *does* enforce audio focus:
  `dumpsys car_service CarAudioService` reports `Use hal ducking signals true`, and the focus
  log shows `sendFocusLoss … LOSS` at 16:01:23 plus a `LOSS_TRANSIENT` to our media client at
  16:15:23. `AacPlayer.requestFocus` attaches no listener and discards the result;
  `VoiceRouter`'s listener only logs. We are being ducked and we ignore it.
- **5.5 → LOW / diagnostic.** The wedge is real but `AvSink` is started only by the `av_sink`
  serial command and is never up during a real session; the real path
  (`CarPlayActivity.stopSession`) does close live sockets.
- **5.6 → keep only the CANCEL half.** Multi-touch teleport is already guarded; the residual
  defect is `ACTION_CANCEL` folded into the `ACTION_UP` arm, delivering an abort to iOS as a
  completed lift.
- **5.13 (d) → soften.** The `CT_SETTIME` ack is absent from the JNI `lib.rs` but present and
  working in `ocbmd`; docs/02 is correct.

## Confirmed at stated severity

0.1 (mechanism), 3.1–3.5, 4.2, 5.1, 5.3, 5.7, 5.9, 5.10, 5.11, 5.12, 5.13 (a)–(c).

**4.2 is confirmed and indefensible as shipped.** **5.1** is confirmed with a self-contradictory
line on hardware: `SESSION … ttff_ms=292075 frames=0 no_video=true exit=host_gone` — a first
frame was observed yet `frames=0`, because `framesRendered` is only written by
`HevcRenderer.stop()`, which never runs on `superseded`/`process_death` and races on `host_gone`.

## Root-cause chain (3.1 + 3.2) — stands, with two wording corrections

Both ends are now direct observation:

- **Head-unit side.** All three 5353 join sites use single-arg `joinGroup(InetAddress)`
  (`MdnsResponder.kt:77`, `MdnsInspect.kt:38`, `:143`). Live: `ip route get 224.0.0.251` →
  `Network is unreachable`; `ip rule` shows `32000: from all unreachable` with no path to a
  table carrying 224/4. **Understated in the first pass:** all three sites *already* call
  `setNetworkInterface()` before the join and still fail — so `IP_MULTICAST_IF` does not feed
  the join on this kernel, which forecloses the cheapest objection to the two-arg fix.
- **Phone side.** 29 `No matching endpoint found for deviceID <PHONE_BT_ID>` lines, cadence
  matching head-unit nudges 1:1 at a constant ~9.3 s clock skew — not cherry-picked. The
  TXT gate is *shown, not inferred*: `airplayd APBonjourCache ### Ignoring device found
  without pairing ID: … 'gm-ccpa'` at 17:26:33, 16:01:41 and 16:17:27. The pairing ID lives in
  TXT, so SRV/A alone do **not** suffice — iOS discards the browse result before endpoint
  creation. In the 17:23–17:26 stall: PTR adds present, TXT `DNSServiceQueryRecord START` ×3,
  **zero results**, and a prior `rmv … type: TXT` at 16:18:51.

**Correction 1 — say "all head-unit 5353 answering stopped", not "the responder died".** The
advert iOS indexes is NsdManager/mdnsd's, and mdnsd kept returning REGISTERED. Why the *system*
daemon stopped serving TXT on br0 is assumed, not shown. This is the chain's one soft link.

**Correction 2 — "TXT expired" → "TXT no longer held and unrefetchable".** The `rmv` event says
`expired: no`; actual expiry is TTL-120 arithmetic, not an observed event. Supporting this:
a dial-back *succeeded* at 16:01:23.925, 61 ms after the first ENODEV — endpoints survive the
network breakage for tens of minutes, so the chain works only via delayed teardown.

**Third nudge failure mode found:** the pending autoconnect has its own expiry
(`pendingAutoconnectID … is expired (2856.245 seconds old)`, ~47.6 min). Recovery logic that
nudges rarely can hit it.

## New findings from the review (not in the first pass)

- **N1 — hardcoded setup code `"3939"`.** A SoftAP-adjacent attacker can self-enroll as a
  trusted controller. Pairs with 0.1.
- **N2 — the box does not run the audited scripts.** Installed `/script/ocbm_boot.sh` is the
  34-line stale copy; the first pass had the "stale duplicate" relationship inverted. The
  deploy dead-man guarding a bad `ocbmd` flash is dead code that was never deployed.
- **N3 — L2 escalation success check lies.** Proven empirically on the box: `pgrep -f ocbmd`
  matches the wrapper (PID 71), so the check reports success with zero daemons alive.
- **N4 — REBOOT_BUDGET write failure → unbounded L3 reboots.** In the *running* code.
- **N5 — `/etc/hostapd.conf` holds `ssid=<AP_SSID>` with a live passphrase**, plus a `.stock`
  backup. Treat the passphrase as compromised; rotate before any repo or log ever carries it.
- **N6 — `CORE` held across blocking JNI upcalls** → ANR risk.
- **N7 — rung 0 is a silent no-op on this head unit.** `reannounce()` delegates to the
  self-hosted responder, which is never up (ENODEV). Every rung-0 pass does nothing, logs
  `reannounce: self-hosted responder is not up`, and **still returns `true`** — consuming the
  rung, its 30 s cooldown and the 20 s retry slot. The ladder's cheapest rung is fictitious in
  exactly the situation the ladder exists for.
- **N8 — overlapping timers double-fire the ladder.** `inboundTimer` expiry moves the phase to
  `ARMED`, re-qualifying the still-armed `handoffTimer`; nothing cancels it on dial-accept.
  Observed at 17:31:17.732 and 17:31:21.243 — two rungs burned in four seconds for one fault,
  which is what promoted the ladder to the destructive rung 2 at 17:31:34.
- **N9 — the supervisor trusts latched `bt_phase`**, guaranteeing a spurious escalation at the
  start of every session.
- **N10 — a box-health regression during `GRACE` destroys the grace hold.** Since rung 1
  demonstrably *causes* health regressions, our own recovery can preempt a session-end wait.
- **N11 — `armRadioOnEdge` ignores the ON-write result.**
- **N12 — the `capture scope=… effective=… read_logs=…` start line is inverted.** It samples
  `LogCapture.status()` synchronously right after `start()`, but both fields are resolved later
  on the pump thread in `resolveScope()`, defaulting to `cfg.scope` and `false`. On hardware:
  17:24:26 printed `effective=WHOLE_OS read_logs=false` while the settled status at 17:28:32
  reported `readLogsGranted=true, effectiveScope=OWN_PROCESS` — **both fields inverted.** An
  operator reads "READ_LOGS denied, whole-OS capture succeeded" when the truth is the opposite.
  This is what fed the 5.2 scope confusion.
- **N13 — `AvSink.stop()` logs "A/V sink stopped" unconditionally** while a pump may still be
  parked in `read()` with bytes arriving.

## Open experiments (none require the iPhone except the last)

1. From this Mac on the head-unit AP: `dns-sd -B _airplay._tcp`, plus a direct unicast TXT
   query at the head unit's br0 address:5353. On the head unit: `cat /proc/net/igmp` (is
   224.0.0.251 joined on br0?) and `ss -ulnp | grep 5353`. **Closes the mdnsd soft link.**
2. Build with `joinGroup(InetSocketAddress(GROUP,5353), br0)` and watch for "responder up".
   **Confirms the fix works on this kernel** — currently untested here.
3. `adb shell ip monitor route` across one full hotspot bring-up/tear-down. **Establishes
   whether route loss recurs per cycle or was a one-off** (the 15:40:27 `LegacyTypeTracker.remove`
   + `iptables -D tetherctrl_raw_PREROUTING -i br0` trigger is a candidate only).
4. After one full BT handshake, leave the box idle 10 min, then `cat /tmp/bt_phase`. A
   non-idle byte is direct proof of 3.3.
5. *(needs iPhone)* Capture one success and confirm `Created APEndpoint … gm-ccpa` follows a
   TXT QueryRecord result. **Closes the positive half of the TXT gate**, currently supported
   only indirectly by other devices' endpoints in the same window.
