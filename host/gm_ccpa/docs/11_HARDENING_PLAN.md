# 11 — Hardening: live status ledger + open work

**What this is.** The status ledger for the hardening effort that started from a 2026-08-09 code
review (Appendix A) and continued through the 2026-08-27/28 lifecycle work and the 2026-08-27
post-audit remediation (R6). The ledger below is the current truth. Everything after it —
release/track structure, verification method, Appendix A — is historical or reference material and
is subordinate to the ledger. Re-verified against source 2026-08-31 (see "Verification method"
per row below: grep/read of `netprobe_app/`, `native/carplay-jni/src/lib.rs`, `ccpa_custom`).

**Governing constraints that still hold:**
- The doc-05/06 proven flow is the regression oracle for every change.
- Reversible on the box: track and restore the patched supervisor / hostapd / `/tmp/no_escalate`
  before any truck test — it is tmpfs and self-clears on reboot.
- GM `CarplayService` coexistence is untested as of this pass; log its state every session.

---

## Status ledger

State legend: **LANDED** (confirmed in source), **OPEN** (not started or not finished), **DEFERRED**
(deliberately not doing yet), **UNVERIFIED** (could not confirm either way this pass).

### App/session lifecycle (R1–R2, 2026-08-09 + 2026-08-27/28 work)

| ID | What | State | Evidence | Device-verified |
|---|---|---|---|---|
| C1/T1.1–1.3 | Generation-checked native handle registry; uniform `catch_unwind`; `feed` null-on-error | LANDED | `native/carplay-jni/src/lib.rs:48` (`CORE` mutex + gen) | No |
| C2/T1.4–1.7 | Connection hijack, per-generation destroy, rediscover-on-session-end | LANDED | `netprobe_app/.../CarPlayRx.kt` (`currentSocket`, `acceptLoop`) | No |
| — | `OcbmProbe.runAll()` returns `LinkResult`, not `Unit` — launcher no longer claims "MFi proven" with no adapter | LANDED | `ocbm/OcbmProbe.kt:230,248` | No |
| — | `SessionHolder` — receiver/probe/sink made process-scoped | LANDED | `MainActivity.kt`, `CarPlayRx.kt` (search `SessionHolder`) | No |
| — | `NativeCore.BUSY` bounds `nativeInit` on CORE contention | LANDED | `pair/NativeCore.kt:56` | No |
| C3/T2.1 | Foreground service + manifest `<service>` | LANDED | `av/CarPlaySessionService.kt` | No |
| T2.2 | Full session-ownership relocation into the service (survive Activity *destruction*, not just backgrounding) | **OPEN** | not found; service exists but the risky slice (moving `cpRx`/`ocbmProbe`/`avSink` ownership) is not confirmed done | No |
| T4.8 | Serialize MainActivity command dispatcher through one executor; guard destructive verbs | LANDED | `MainActivity.kt` `cmdExecutor` (single-thread) | No |
| — | `CarPlayActivity.onSessionEnded()` tears the screen down on session end | LANDED | `av/CarPlayActivity.kt:77` | Partial — device-observed cold-start bug this fixed; teardown path itself not separately re-driven |
| — | `AacPlayer.reclaimFocus()` recovers media after permanent `AUDIOFOCUS_LOSS` | LANDED | `av/AacPlayer.kt:174` | No |
| — | `AacPlayer` full focus-listener + duck/pause/restore (R6.7 item 31) | LANDED | `av/AacPlayer.kt:117-225` (`AudioFocusRequest`, `focusListener`) | No |
| C4/M3/T3.1–3.4 | `OcbmClient` `mfiLock`, length-correlated MFi responses, `Mfi.parse` truncation reject, `Tlv8` consecutive-only coalescing | LANDED | `ocbm/OcbmClient.kt`, `ocbm/OcbmProto.kt` | No |
| T5.3 | Gate `mfi-i2c-local` behind `local-mfi` (off on Android); route `iap_tunnel` through `Arc<Mutex<dyn MfiSigner>>` | LANDED | `ccpa_custom crates/vendor/receiver/Cargo.toml:56,60`; `native/carplay-jni/src/lib.rs:468` (`set_remote_signer`) | No |
| — | Box-log streaming over `CH_FILE` | LANDED | `ocbm/OcbmProbe.kt:656-674`, `ocbm/OcbmClient.kt:507-567` | **Yes** |
| T5.1 | `JNI_OnLoad` env-var config → one explicit `nativeInit` config; `/info` generated from same config | **OPEN** | `native/carplay-jni/src/lib.rs:351-370` still `std::env::set_var(...)` | No |
| M7/T5.4 | Rust owns the control `TcpListener`/accept loop | **DEFERRED** (by design — hardware-gated) | no `TcpListener` in `lib.rs` | No |
| T6.4 | Gate `CARPLAY_SCREEN_DUMP`/`CARPLAY_SETUP_DUMP`/`.verbose(true)` behind `BuildConfig.DEBUG` | **OPEN** | `lib.rs:370` still unconditional `set_var("CARPLAY_SCREEN_DUMP","1")` | No |
| T6.5 | `SecureRandom` `edSeed`, persisted beside peer store (invalidates pairings — ships alone) | **OPEN** | `CarPlayRx.kt:84` still `ByteArray(32) { (it*31+7).toByte() }` | No |
| T0.1 | Pin `ccpa_custom` as a submodule/vendored snapshot, replacing the bare path dep | **OPEN** | `native/carplay-jni/Cargo.toml:39-41` still `path = "../../../ccpa_custom/..."`; the "LAST REVIEWED REV" comment is manual, not enforced | No |
| — | Build-time check that fails/warns on `ccpa_custom` HEAD ≠ reviewed rev | **OPEN** | `tools/build_apk.sh:135` only stamps the *gm_ccpa* SHA, no `ccpa_custom` rev-compare | No |
| T0.2/T0.3/T0.5 | R8 keep-rule for `MfiRelay`; `build_apk.sh` → android-32, fail-fast, `pipefail`; `Cargo.toml` lto/strip/codegen-units | LANDED | `netprobe_app/app/proguard-rules.pro:7-8`; `tools/build_apk.sh:3,10`; `Cargo.toml:60-64` | No |
| T6.1 | Delete `AirPlayRx.kt` + wirings | LANDED | absent from tree | No |
| T6.3 | Delete dead code (`unsafe impl Send for Native`, `_UNUSED`, `nativeReady`, etc.) | LANDED | `unsafe impl Send for Native` absent from `lib.rs` | No |
| 5.6 | `ACTION_CANCEL` emits a MOVE-then-slop instead of a phantom tap | LANDED | `av/CarPlayActivity.kt:426` (`PHASE_CANCEL`) | No |
| 5.10 | `SessionSupervisor`'s `server` field made `@Volatile`, bound before dispatch | LANDED | `CarPlayRx.kt:129,299-313` | No |
| 4.2/N8 | Recovery ladder: cooling rung waits instead of promoting; `handoffTimer` cancelled in accept path | LANDED | `SessionSupervisor.escalate`, `onDialAccepted` | No |
| 4.2/N8b | Recovery ladder stands its blind 20 s retry down on forward progress (`deferLadder`), so rung 2 cannot fire into a live handshake — device-observed tearing down a healthy session twice, 2026-09-08 | LANDED 2026-09-08 | `SessionSupervisor.deferLadder`, `onBtPhase`, `onDialAccepted`; `04_SYSTEM_MODEL.md` §"The recovery ladder" | **Not yet** — needs a run where a rung fires and a handshake starts inside the 20 s window |
| Leak fixes | `MainActivity` command executor and `OcbmProbe` executor now `shutdown()` on teardown | LANDED | `MainActivity.kt:843`, `OcbmProbe.kt:634` | No |
| 5.12 | Build-time test asserting `/info`↔TXT equality | **OPEN** | no test file found for this | No |
| 5.2 | Per-scope (not shared) sweep budget in `LogCapture` | **OPEN**, low urgency (nothing lost in the corpus) | `logging/LogCapture.kt:262` still filters by shared `FILE_PREFIX` | No |
| N6 | Drop `CORE` lock across the blocking JNI upcall | **OPEN** — flagged in source as needing its own `deep`-tier pass | not attempted | No |
| HEVC reader/decoder thread split (T4.5 residual) | Separate read/decode threads behind `hevc.reader.thread` flag | **OPEN** | no reader-thread flag found in `av/HevcRenderer.kt` | No |

### Discovery / mDNS (R6.1–R6.2, 2026-08-27)

| ID | What | State | Evidence | Device-verified |
|---|---|---|---|---|
| N12 | `capture scope=… effective=… read_logs=…` prints only resolved values, on the pump thread | LANDED | `logging/LogCapture.kt:145-147,395,425` (`resolved: Boolean`) | **Yes** (2026-08-27 deploy log) |
| A1 | Two-arg `joinGroup` at all three 5353 sites | LANDED | `MdnsResponder.kt:90`, `MdnsInspect.kt:43,154` | **Yes** — br0 multicast users 1→2, `ENODEV` gone |
| — | mdnsd leg decision | RESOLVED — A1 alone was sufficient; speculative NsdManager re-registration dropped, not built | doc record only | **Yes** |
| RFC 6762 hygiene | AAAA answered with A + NSEC; announce spacing 250 ms → 1 s | LANDED | `MdnsResponder.kt:190-197,281` | Partial (A1 verified; AAAA/NSEC path not separately isolated) |
| — | Capture always-on from `onCreate` | LANDED | (`LogCapture.start` wired) | **Yes** |

### Box side (`ccpa_custom`)

| ID | What | State | Evidence | Device-verified |
|---|---|---|---|---|
| Radio seam | `radio_hal.sh`/`radio_detect.sh` missing from deployed supervisor → no BT bring-up, no error surfaced | **LANDED (fixed 2026-08-28)** | `ccpa_custom docs/ops/06_CORRECTIONS_LEDGER.md` `R-20W-5`; `tools/ocbm_push.sh:73-95` now warns on a partial push | **Yes** — `hci0 UP RUNNING` from cold boot after the two scripts were pushed |
| N3 | `pgrep -x`/`pkill -x ocbmd` (was `-f`, matched the respawn wrapper) | LANDED | `ccpa_custom tools/session_supervisor.sh:460,467` | Unverified this pass |
| N4 | Reboot budget write-verified after `sync`, fails closed | LANDED | `session_supervisor.sh:417-422` | Unverified this pass |
| 3.3 | `/tmp/bt_phase` un-latched (`rm -f` on exit/teardown/start) | LANDED | `session_supervisor.sh:879` | Unverified this pass |
| 5.7 | BT SYN resend capped at 10 total | LANDED | `crates/vendor/wireless/src/bt_driver.rs:141-226` (`SYN_RESEND_MAX = 9`, "audit 5.7") | Unverified this pass |
| 3.4/3.5 | Split HOST_GONE emission; `F_REPLAY` bit2 semantics | **UNVERIFIED** | could not locate the source module (not under `crates/` or `tools/` in this checkout) | No |
| R6.5 | TTL-gated deploy trial / dead-man before flashing a daemon | Process step, not code — treat as standing procedure, not a landed/open item | — | — |
| R6.6.27–28 | Rotate AP passphrase; rotate `edSeed`/setup code fallback | **OPEN** | `crates/vendor/pairing/src/srp.rs`, `crates/vendor/rx-connect/src/main.rs:29` still carry the hardcoded `b"3939"` / `PI_FALLBACK` | No |
| R6.6.29 | Scrub tracked credential files, widen `RE_WPA` redactor | **OPEN** | `ccpa_custom/pi/evidence/hostapd_5g.conf` still tracked; no `RE_WPA` symbol found in this checkout | No |

**On the two open box-side credential items (R6.6.27–29): these are real, unremediated exposures in
a sibling repo (`ccpa_custom`). This doc does not fix them (out of scope for this file) — flagging
here so the ledger doesn't imply otherwise. Do not print the actual passphrase/seed value into any
future doc; use a placeholder and rotate before any further sharing of that repo.**

---

## Open work, detail

Kept here because a one-line ledger entry isn't enough to act on.

- **T0.1 — pin `ccpa_custom`.** Still a bare relative path dep (`Cargo.toml:39-41`); the "LAST
  REVIEWED REV" comment is manually maintained, not enforced. Add either a submodule/vendored
  snapshot, or at minimum a build-time `git -C ../../../ccpa_custom rev-parse --short HEAD` compare
  against a `REVIEWED_REV` constant that fails the build (or stamps a loud banner) on mismatch.
  Until then, diff `ccpa_custom` by hand at the start of every session — see "Upstream drift" below.
- **T2.2 — full session-ownership relocation into `CarPlaySessionService`.** The service exists and
  owns process priority; whether `cpRx`/`ocbmProbe`/`avSink` construction itself now lives inside it
  (survives Activity *destruction*, not just backgrounding) was not confirmed this pass — read
  `CarPlaySessionService.kt` before assuming it's done. Land behind a `service.owner` flag if not:
  empty FGS → AvSink/AacPlayer → OCBM → native core, A/V last (re-attach must preserve both the
  `@Volatile` renderer publish and the `:9001` socket close that forces producer re-dial).
- **T5.1 — env-var config → `nativeInit` config.** Still the `−16720` desync class: `/info` is
  static-baked while `CARPLAY_SESSION_MGMT` etc. are env vars set in `JNI_OnLoad`
  (`lib.rs:351-370`). Fix: one explicit config struct through `nativeInit`; generate `/info` in Rust
  from the same config; keep both paths with a startup byte-compare assert for ≥1 milestone before
  deleting the env path.
- **T6.4 — gate debug dumps.** `CARPLAY_SCREEN_DUMP`/`CARPLAY_SETUP_DUMP`/`.verbose(true)` are always
  on (`lib.rs:370`) → unbounded disk growth in production. Gate behind `BuildConfig.DEBUG`.
- **T6.5 — rotate `edSeed`.** Still `(it*31+7).toByte()` (`CarPlayRx.kt:84`) — deterministic, derivable
  from source, identical across every install; the accessory's Ed25519 long-term key is not secret.
  Generate once with `SecureRandom`, persist beside `carplay_peers.bin`. **Ships alone, truck-gated —
  invalidates every existing pairing.**
- **M7/T5.4 — Rust owns the control listener.** Deferred by design: it replaces the one
  device-proven accept path and re-times the advertise-before-dial race. `receiver::net::serve_connection`
  is already shaped for it. Needs a re-authored ~100-line accept loop (not a port — `run_pairing_server`
  isn't in this checkout) plus a new upward `SessionListener` callback so Kotlin doesn't lose the
  accept event that rediscover depends on. Attempt only after everything above is truck-proven, one
  variable at a time.
- **5.12 — `/info`↔TXT build-time test.** No test asserts `info.bplist` against the Kotlin TXT
  constants; a desync still only fails on hardware as pair-verify `-16720`.
- **5.2 — per-scope sweep budget.** `LogCapture`'s 64 MB ceiling is shared across scopes instead of
  per-`Scope`. Low urgency — no `ceiling: dropped` line has ever appeared in the corpus, max on-disk
  observed is 7.69 MB.
- **N6 — drop `CORE` across the blocking JNI upcall.** Flagged in source as genuinely subtle;
  implement at `deep` tier, its own commit. The motivating "10 s phone timeout" figure is unsourced
  (only exists in 2026-08-27 comments) — the held-lock structure is real, the urgency claim was not.
- **HEVC reader/decoder thread split.** Socket read and codec feed still share one thread in
  `HevcRenderer`; no `hevc.reader.thread` flag exists. A >2 s decode stall still trips the producer's
  write timeout. Fix: reader thread + bounded queue, drop-oldest of whole AUs only, never a
  keyframe/param-set-carrying AU.
- **Box: 3.4/3.5 (HOST_GONE split, `F_REPLAY` bit2), R6.6 credential rotation/scrub.** See ledger
  above — unverified or open, in `ccpa_custom`, not this repo.

---

## Nothing in the recent app work is device-verified except two items

Per the ledger: **box-log streaming (`CH_FILE`)** and the **A1 mDNS `joinGroup` fix** are the only
2026-08-27+ app changes confirmed on hardware. Everything else marked LANDED in the ledger above
compiled and was reviewed against source but has not been separately re-driven on the truck. Treat
"LANDED" and "device-verified" as two different claims — the ledger keeps them in separate columns
for that reason.

---

## Upstream `ccpa_custom` drift — standing procedure

`gm_ccpa` links `receiver`/`pairing`/`mfi` as **path deps** (compiles the working tree, not a pinned
rev — see T0.1 above). Last manual review was at `6bc326d` (2026-08-12); re-check before trusting any
later build:

1. `git -C ../ccpa_custom rev-parse --short HEAD`, diff against the reviewed rev.
2. Re-scan `receiver`/`pairing`/`mfi`/`ocbm-proto` for changes to `forward.rs`/`stream.rs`/
   `server.rs`/`net.rs`/`datastream.rs`/`iap_tunnel.rs` (the `:9001`/`:9002` seam contract) and to
   `levers.rs`/`session.rs` (the `OCBM_FWD_ENC` and lever-write contract).
3. **Known-safe fact, re-verify on every advance:** `OCBM_FWD_ENC=0` is set in `JNI_OnLoad`
   (`lib.rs:351`) and `levers::fwd_enc()` treats `0`/`false`/`off`/empty as opt-out. gm_ccpa's
   one-shot `levers::set_hevc(true)` etc. stands only because gm_ccpa never calls
   `VehicleConfig::apply()` — if that ever changes, `hevc` can silently drop out of the SETUP
   `enabledFeatures` echo while the static `/info` still advertises `hevcInfo` (a half-satisfied
   gate — the exact shape of a session that dies ~21 ms after RECORD).

---

## Reference: release/track structure (executed, kept for context)

The work was originally sequenced as 5 releases across 3 parallel tracks (session/native,
OCBM/USB link, A/V consumers) plus a deferred R5 (Rust-owned listener). R0–R4 and R6 are executed;
see the ledger above for what actually landed vs. what's still open within each. R5/M7 remains
deferred by design.

**Verification tiers used throughout:** Tier 0 — host `cargo test -p receiver -p pairing`, wired into
the build gate. Tier 1 — host replay binary driving `serve_connection`/`ControlServer::handle` over
fixture captures. Tier 2 — emulator lifecycle exerciser (hijack, 500× connect/kill/restart UAF hunt
with CheckJNI on, service-survives-Activity-finish). **T-REG** (`tools/truck_reg.sh`, ~15 min) is the
standard truck regression: revive → force-stop → full run → phase-2 SETUPs → first-frame/first-audio
assertions → touch → 60 s soak → socket check → teardown with 0 AUs dropped. Run T-REG on the golden
APK first every visit to prove the rig before blaming the candidate.

**Regression risk register (still relevant if T2.2/T5.1/M7 are picked up):** the service-ownership
move is highest risk (re-homes every working object, can't be fully proven off-truck — slice behind a
flag); the handle registry's risk was a silent stall, not a crash (mitigated by the two-commit
INC-1/INC-2 split, already landed); M7 discards the only device-proven accept loop (mitigated by
keeping the Kotlin listener runtime-selectable); the HEVC split's failure mode is invisible
stutter, not a crash (mitigated by never dropping a keyframe/param-set AU); T5.1 reintroduces the
`-16720` desync class (mitigated by the dual-path assert, never deleting the old path in the same
commit).

---

## R6.10 — box BT regression, 2026-08-27 evening (RESOLVED 2026-08-28)

A deploy that pushed `session_supervisor.sh` (969→1227 lines) and `ocbmd`/`carplay-wireless` binaries
left the box with `BOX_HEALTH 0x50` — `HCI_PRESENT` bit clear, no Bluetooth radio, no pairing
possible — while OCBM claim/HELLO/MFi/SUBSCRIBE all still reported success with no error anywhere.

**Root cause, confirmed on hardware:** `radio_hal.sh`/`radio_detect.sh` were absent from the deployed
unit (`ls` returned "No such file or directory"). The supervisor invokes them inside a detached
`setsid` wrapper and never reads the exit status, so the missing scripts produced no signal. Fix:
push the two scripts; `hci0` came up `UP RUNNING` from a cold boot. See `ccpa_custom docs/ops/06_CORRECTIONS_LEDGER.md`
`R-20W-5` for the full account. **Process fix landed:** `tools/ocbm_push.sh` now warns when a
supervisor push is missing the radio seam (`tools/ocbm_push.sh:73-95`).

**Diagnostic shortcut for next time:** read `CT_BOX_HEALTH` bit 0 (`BH_HCI_PRESENT`) first —
`0x50` with bit 0 clear is this exact signature.

**Process gap noted at the time, still true going forward:** the replaced `session_supervisor.sh` was
overwritten without a backup; only its md5 was recorded. Do not overwrite a box file again without
saving the original first.

---

# APPENDIX A — the 2026-08-09 review this plan came from (compressed)

> The status ledger above is current truth and outranks everything below. This appendix is kept only
> for reasoning that still guides open work (the language/boundary verdict, and why the MFi signer
> needed an `Arc<Mutex<>>`). Every CRITICAL/HIGH/MEDIUM/LOW finding from the original review maps
> 1:1 to a task ID in the ledger above and has been re-adjudicated there — do not act on a severity
> from this appendix without checking the ledger first.

**Headline.** The architecture was sound; defects clustered into five cross-cutting themes, closed by
one structural fix (a session object with a generation counter, shared across the Kotlin↔Rust
boundary) plus config unification, build reproducibility, and a failure-surfacing pass. Wire
protocol, SRP-6a crypto, and OCBM framing were verified correct throughout — the gaps were in
lifecycle, failure surfacing, and build reproducibility, not protocol correctness.

**Language/architecture verdict — settled, still governs.** Keep the Kotlin-shell + JNI'd-Rust-core
split; refine it, don't rewrite it. Three independent lenses agreed (neutral judge: keep-and-refine
4.08 vs all-Rust 3.28 vs all-Kotlin 3.18). All-Kotlin is the only real alternative and is dominated by
the cost of re-proving ~19k loc of iPhone-validated protocol behavior (nine silent-failure modes) on
a truck where logcat is the only debug channel; all-Rust fights Java-only framework APIs
(`MediaCodec`/`AudioTrack`/`NsdManager`/`UsbManager` — NsdManager was empirically the only thing that
entered iOS's endpoint index).

**Why Rust must eventually own the control listener (bears on M7 — still deferred).**
`receiver/src/session.rs` binds every data-plane socket itself; only the single control connection is
external, through `receiver::net::serve_connection<T: Read+Write>`. Moving the accept loop into Rust
recovers connection-hijack and `TCP_KEEPIDLE/INTVL/CNT` dead-link detection (which `java.net` cannot
express) and lets `nativeFeed`/`nativeIsEncrypted` disappear. `run_pairing_server` is not in this
checkout — the accept loop must be re-authored, not ported — and needs a new upward
`SessionListener` callback so Kotlin doesn't lose the accept event that rediscover depends on.

**Why the `iap_tunnel` MFi routing needed a signature change, not a call-site edit (T5.3, now
landed).** `ControlServer` owned its signer by value and `iap_tunnel` ran on a spawned thread, not
inside `handle()` — routing its two chip-call sites through one signer required threading an
`Arc<Mutex<dyn MfiSigner + Send>>` through, which is what shipped. This also meant C4 (the CH_MFI
correlation gap) had to land first, since routing the second consumer through the client is what made
the correlation bug live rather than latent.

**Doc-correction note (already actioned):** the review flagged `03`/`lib.rs` as overstating "API 32 is
why the core is Rust at all" — true but not decisive; the deciding factor was the 19k loc of validated
protocol. Wording softened.

**Outstanding platform work carried forward from the retired `07_FORWARD_PLAN.md`:**
1. **A/V suppression on the box is a workaround, not a fix.** `airplayd`/`rx-connect` are pointed at
   `/bin/true` rather than gated by an explicit `AV_DISABLED` in `av.rs` — harmless only because the
   bridge role has no `wlan0`/route to the phone.
2. **Box identity is degenerate in the bridge role.** `box_identity::derive()` falls through to
   `/proc/cpuinfo` Serial, which reads all-zero here — every adapter derives the same identity.
   Harmless with one adapter; must be fixed before a second exists.
3. **Restore test inhibits before shipping:** `/tmp/no_escalate` (tmpfs), patched
   `session_supervisor.sh`, `/etc/hostapd.conf` (restore from `.stock`).
4. **GM coexistence is permanent and untested this pass.** Log `CarplayService`/`:7000` state every
   session.

**Risks that outlived the plan:**

| Risk | Mitigation |
|---|---|
| Identity drift across four representations (TXT `features`, `/info` `features`, `deviceid`/`pi`, decimal MAC in the connect-out header) | A golden-identity test deriving all four from one source and byte-comparing |
| Silent-failure protocol traps — nine failure modes produce no diagnostic | Log every unhandled route loudly; never answer with a fake 200 |
| Negotiating unimplemented features kills the session ~21 ms after RECORD | Implement, then advertise — never the reverse |
| iPhone log redaction (`<private>` without Apple's CarPlay logging profile) | Preflight assertion: fail loudly if a known reason string comes back redacted |
| Bluetooth is the single point of failure — no cable fallback | Keep BT health visible |
| `/tmp/no_escalate` is tmpfs; one flap re-arms the reboot ladder | Re-assert every session start; power-cycle the adapter, not the truck |
