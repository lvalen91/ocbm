# 11 — Hardening: live status ledger + open work

**What this is.** The status ledger for the hardening effort that started from a 2026-08-09 code
review (Appendix A) and continued through the 2026-08-27/28 lifecycle work and the 2026-08-27
post-audit remediation (R6). The ledger below is the current truth. Everything after it —
release/track structure, verification method, Appendix A — is historical or reference material and
is subordinate to the ledger. Re-verified against source 2026-08-31 (see "Verification method"
per row below: grep/read of `netprobe_app/`, `native/carplay-jni/src/lib.rs`, `ccpa_custom`).

**Governing constraints that still hold:**
- **`crates/` and `ccpa/` Rust is STATIC. Do not change it.** *(Owner, 2026-09-10.)* That code is
  proven on the box and in the macOS host, and this app is one of three consumers — `gm_ccpa` adapts
  to it, never the reverse. Where the honest fix is upstream, record it as a follow-up and solve it
  on the Kotlin side or not at all. This is why A11 landed Kotlin-only with zero Rust changes, and
  why the cheaper alternative it identified (a `speechMode` atomic beside `SCREEN_FOCUSED` plus a
  `nativeSpeechMode()` export) was written down rather than built. Rows written before this
  constraint that assume upstream can move — **N13, N14, N17** — are re-scoped below.
  The one exception taken so far is `c39c9af`, which corrected four assertions inside
  `#[cfg(test)] mod tests` in `receiver`; test modules are not compiled into any release artifact,
  so no shipping binary differs. If even that is unwanted, revert it and accept a red Tier-0.
- The doc-05/06 proven flow is the regression oracle for every change.
- Reversible on the box: track and restore the patched supervisor / hostapd / `/tmp/no_escalate`
  before any truck test — it is tmpfs and self-clears on reboot.
- ~~GM `CarplayService` coexistence is untested as of this pass~~ — **proven 2026-09-09**: a full
  A/V session ran with GM's service live and untouched, every other GM CarPlay/projection component
  disabled or uninstalled. Still log its state every session.

---

## Status ledger

State legend: **LANDED** (confirmed in source), **OPEN** (not started or not finished), **DEFERRED**
(deliberately not doing yet), **UNVERIFIED** (could not confirm either way this pass).

### App/session lifecycle (R1–R2, 2026-08-09 + 2026-08-27/28 work)

| ID | What | State | Evidence | Device-verified |
|---|---|---|---|---|
| C1/T1.1–1.3 | Generation-checked native handle registry; uniform `catch_unwind`; `feed` null-on-error | LANDED | `static CORE` / `fn core()` in `native/carplay-jni/src/lib.rs` (mutex + gen) | No |
| C2/T1.4–1.7 | Connection hijack, per-generation destroy, rediscover-on-session-end | LANDED | `netprobe_app/.../CarPlayRx.kt` (`incumbent`: `IncumbentSlot` in `CarPlayRxSlots.kt`, was `currentSocket` until v5.7 F040; `acceptLoop`) | No |
| — | `OcbmProbe.runAll()` returns `LinkResult`, not `Unit` — launcher no longer claims "MFi proven" with no adapter | LANDED | `OcbmProbe.LinkResult` (`ocbm/OcbmProbe.kt`) | No |
| — | `SessionHolder` — receiver/probe/sink made process-scoped | LANDED | `MainActivity.kt`, `CarPlayRx.kt` (search `SessionHolder`) | No |
| — | `NativeCore.BUSY` bounds `nativeInit` on CORE contention | LANDED | `NativeCore.BUSY` (`pair/NativeCore.kt`) | No |
| C3/T2.1 | Foreground service + manifest `<service>` | LANDED | `av/CarPlaySessionService.kt` | No |
| T2.2 | Full session-ownership relocation into the service (survive Activity *destruction*, not just backgrounding) — scoped to `cpRx`/`ocbmProbe` only; does NOT cover re-seeding the per-Activity observers (`MainActivity.sessionUp`, `VehicleStateWatcher`, the supervisor's phase) | **OPEN** | not found; service exists but the risky slice (moving `cpRx`/`ocbmProbe` ownership) is not confirmed done | No |
| T4.8 | Serialize MainActivity command dispatcher through one executor; guard destructive verbs | LANDED | `MainActivity.kt` `cmdExecutor` (single-thread) | No |
| — | `CarPlayActivity.onSessionEnded()` tears the screen down on session end | LANDED | companion `onSessionEnded` in `av/CarPlayActivity.kt` | Partial — device-observed cold-start bug this fixed; teardown path itself not separately re-driven |
| — | `AacPlayer.reclaimFocus()` recovers media after permanent `AUDIOFOCUS_LOSS` | LANDED | `av/AacPlayer.kt` `reclaimFocus()` | No |
| — | `AacPlayer` full focus-listener + duck/pause/restore (R6.7 item 31) | LANDED | `av/AacPlayer.kt`: the `AudioFocusRequest.Builder(...)...setOnAudioFocusChangeListener(focusListener)` call in `requestFocus()`, and the `focusListener` lambda (ends just above `focusName()`) | No |
| C4/M3/T3.1–3.4 | `OcbmClient` `mfiLock`, length-correlated MFi responses, `Mfi.parse` truncation reject, `Tlv8` consecutive-only coalescing (`Tlv8` part: removed v5.9 D016 — `pair/Tlv8.kt` deleted with the Kotlin pair-setup stub) | LANDED | `ocbm/OcbmClient.kt`, `ocbm/OcbmProto.kt` | No |
| T5.3 | Gate `mfi-i2c-local` behind `local-mfi` (off on Android); route `iap_tunnel` through `Arc<Mutex<dyn MfiSigner>>` | LANDED | `receiver`'s `local-mfi` feature in `ccpa_custom crates/vendor/receiver/Cargo.toml`; the `receiver::iap_tunnel::set_remote_signer(...)` call in `Java_zeno_gmccpa_pair_NativeCore_nativeInit` (`native/carplay-jni/src/lib.rs`) | **Yes — 2026-09-09.** Tunnel got real `0xAA01`/`0xAA03` round trips through the OCBM relay (945 B cert, 128 B sig), reached `Identified`, and subscribed all three metadata channels. Zero `lock busy` lines. See `12_OBSERVED_FLOW.md` |
| — | Box-log streaming into logcat — **now `CH_LOG` push, not `CH_FILE` poll** (relabelled 2026-09-10: the row's title lagged the mechanism). Live streaming is `OcbmClient.handleLog` armed by `OcbmProbe.startBoxLogStream()`; `CH_FILE` survives only as the end-of-session snapshot | LANDED | `OcbmProbe.captureBoxLogsNow()` (`ocbm/OcbmProbe.kt`), `OcbmClient.filePull()` (`ocbm/OcbmClient.kt`); stale line cite, re-anchored 2026-09-10 — the old ranges are now `OcbmProbe.stop()` and `OcbmClient.startHeartbeat()` | **Yes** |
| T5.1 | `JNI_OnLoad` env-var config → one explicit `nativeInit` config; `/info` generated from same config | **OPEN** | the `std::env::set_var(...)` block in `JNI_OnLoad` (`native/carplay-jni/src/lib.rs`) still present | No |
| M7/T5.4 | Rust owns the control `TcpListener`/accept loop | **DEFERRED** (by design — hardware-gated) | no `TcpListener` in `lib.rs` | No |
| T6.4 | Gate `CARPLAY_SCREEN_DUMP`/`CARPLAY_SETUP_DUMP`/`.verbose(true)` behind a debug flag | **CLOSED 2026-09-10 — the premise was wrong, do not gate these** | The unconditional `std::env::set_var("CARPLAY_SCREEN_DUMP", "1")` in `JNI_OnLoad` (`native/carplay-jni/src/lib.rs`). **RETRACTED: it is NOT per-frame.** `spawn_screen` in `crates/vendor/receiver/src/session.rs` guards the dump with `if screen_dump && frames < 6` — at most **six lines per session**, and the 2026-09-09 capture contains exactly six (`DUMP frame#0` … `#5`). The earlier claim in this row, that it writes a hex line per video frame and is per-frame log volume on the A/V path, was a wrong generalisation from seeing frames 0–5 in a grep; commit `89c5c97`'s message repeats it and is likewise wrong. Those six lines are the only in-band codec identity a capture carries (they are what proved `hvc1` and the sample-description wrapping), so gating them off by default would remove evidence for no volume saving. `.verbose(true)` on `ControlServer` gates control-plane lines only — one per request plus pair-setup/verify and the `auth-setup (MFi-SAP) OK/FAILED` verdict, which is the receiver's own word on the `CH_MFI` relay — and is likewise kept. `CARPLAY_SETUP_DUMP` remains deliberately unset for the reason its own comment gives (unbounded per-SETUP files in an unreadable private dir) | Bound verified on hardware: 6 lines in a 1358-frame session |
| T6.5 | `SecureRandom` `edSeed`, persisted per install (invalidates pairings — ships alone) | **LANDED 2026-10-08 (plan v5.8 V5, Q001)** | `AccessoryKey.kt`: 32 `SecureRandom` bytes in `noBackupFilesDir/carplay_accessory_ed25519.seed`, created once under an `O_EXCL` claim (the plan's `Files.move`-without-`REPLACE_EXISTING` was shown non-atomic by the race test and replaced); `CarPlayRx.edSeed` has NO default any more, the only build site (`SessionCore.toggleCarPlayRx`) passes `AccessoryKey.loadOrCreate(...)`. A seed created over an existing `carplay_peers.bin` (an upgrade), or a corrupt/legacy-pattern file, persists a rekey flag: OD-8B runs Forget Pairing automatically once on the next OCBM link and keeps a launcher hint up until a pair-verify succeeds under the new key. Only `created`/`loaded`/one E are logged, never key material. Tier-0: `AccessoryKeyTest` (incl. 8-thread races and mutation checks in it_v58_V5.sh) | Host JVM only; bench B-V5-1 pending |
| T0.1 | Pin `ccpa_custom` as a submodule/vendored snapshot, replacing the bare path dep | **OPEN** | the `receiver`/`pairing`/`mfi` `[dependencies]` entries in `native/carplay-jni/Cargo.toml` still bare relative path deps. **Literal corrected 2026-09-10:** they read `path = "../../../../crates/vendor/receiver"` and siblings, not the `"../../../ccpa_custom/..."` this row used to quote — `host/gm_ccpa` now lives *inside* the `ccpa_custom` checkout, so the path no longer names it. The substantive point stands: unpinned, and the "LAST REVIEWED REV" comment is manual, not enforced | No |
| — | Build-time check that fails/warns on `ccpa_custom` HEAD ≠ reviewed rev | **OPEN** | **MOOT in the current layout — reframed 2026-09-10.** This row assumed two repos. There is one: `git -C host/gm_ccpa rev-parse --show-toplevel` resolves to `ccpa_custom`, and `build_apk.sh` sets `REPO="$GM_ROOT"`, so the SHA it stamps already covers both the app and the vendored crates. The gm_ccpa-vs-ccpa_custom rev distinction this row was written against no longer exists. What DOES remain is T0.1's real hazard — the crates are compiled from the working tree, so a dirty or advanced `crates/vendor/` is invisible in that SHA except for the `-dirty` suffix | n/a |
| T0.2/T0.3/T0.5 | R8 keep-rule for `MfiRelay`; `build_apk.sh` → android-32, fail-fast, `pipefail`; `Cargo.toml` lto/strip/codegen-units | LANDED | the `MfiRelay` `-keep` rules in `netprobe_app/app/proguard-rules.pro`; `ANDJAR`/`CARJAR` pointing at `platforms/android-32` and `set -euo pipefail` in `tools/build_apk.sh`; `[profile.release]` in `Cargo.toml` | No |
| T6.1 | Delete `AirPlayRx.kt` + wirings | LANDED | absent from tree | No |
| T6.3 | Delete dead code (`unsafe impl Send for Native`, `_UNUSED`, `nativeReady`, etc.) | LANDED | `unsafe impl Send for Native` absent from `lib.rs` | No |
| 5.6 | `ACTION_CANCEL` emits a MOVE-then-slop instead of a phantom tap | LANDED | `av/CarPlayActivity.kt`: `PHASE_CANCEL` const, the `MotionEvent.ACTION_CANCEL -> { primaryPointerId = -1; PHASE_CANCEL }` arm in `onTouch`, and the `phase == PHASE_CANCEL` consumer branch | No |
| 5.10 | `SessionSupervisor`'s `server` field made `@Volatile`, bound before dispatch | LANDED | `@Volatile private var server` and the `BIND HERE, NOT ON THE POOL THREAD` block in `CarPlayRx.start()` | No |
| 4.2/N8 | Recovery ladder: cooling rung waits instead of promoting; `handoffTimer` cancelled in accept path | LANDED | `SessionSupervisor.escalate`, `onDialAccepted` | No |
| 4.2/N8b | Recovery ladder stands its blind 20 s retry down on forward progress (`deferLadder`), so rung 2 cannot fire into a live handshake — device-observed tearing down a healthy session twice, 2026-09-08 | LANDED 2026-09-08 | `SessionSupervisor.deferLadder`, `onBtPhase`, `onDialAccepted`; `04_SYSTEM_MODEL.md` §"The recovery ladder" | **Not yet** — needs a run where a rung fires and a handshake starts inside the 20 s window |
| Leak fixes | `MainActivity` command executor and `OcbmProbe` executor now `shutdown()` on teardown | LANDED | `MainActivity.kt` `cmdExecutor.shutdown()`; `ocbm/OcbmProbe.kt` `ops.shutdown()` | No |
| 5.12 | Build-time test asserting `/info`↔TXT equality | **OPEN** | no test file found for this | No |
| 5.2 | Per-scope (not shared) sweep budget in `LogCapture` | LANDED | `LogFiles.sweep()` (`logging/LogFiles.kt`; ages/ceilings each scope, `mine`/`otherNamePrefix`, under its own `Budget`) | No |
| N6 | Drop `CORE` lock across the blocking JNI upcall | **OPEN** — flagged in source as needing its own `deep`-tier pass | not attempted | No |
| HEVC reader/decoder thread split (T4.5 residual) | Separate read/decode threads behind `hevc.reader.thread` flag | **OPEN** | no reader-thread flag found in `av/HevcRenderer.kt` | No |
| N13 | TCP keepalive not armed **on the Kotlin path**: `java.net.Socket` cannot express keepalive idle/interval/count; `android.system.Os.setsockoptInt` can. The reference arms 3 s idle / 3 s interval / 3 probes (~12 s dead-link detect) | **IMPLEMENTED 2026-10-06 (v5.5) — pending on-device acceptance.** Was OPEN (found 2026-09-09, previously untracked; scope corrected 2026-09-09; evidence re-cited 2026-09-10); corrected in place 2026-10-06 | `CarPlayRx.armKeepalive`, armed once per accepted control connection in `handleNative`: `SO_KEEPALIVE`, then `TCP_KEEPIDLE`/`TCP_KEEPINTVL`/`TCP_KEEPCNT` = 3/3/3 and `TCP_USER_TIMEOUT` = 12 000 ms via `Os.setsockoptInt` on a `ParcelFileDescriptor.fromSocket` dup (minSdk 32, where `fromSocket` dups the fd, so closing it leaves the socket and its options intact). Logs `control keepalive armed (3s/3s/3 + user-timeout 12s …)`. The `NOT YET IMPLEMENTED` comment block that tracked this gap is removed. Reference: `fn arm_keepalive` (`ccpa/carplayd/src/main.rs`), called per control connection in `run_pairing_server` | No — pending acceptance |
| N14 | `start_input_listener()` HID endpoint on `127.0.0.1:9110` not implemented **on the Kotlin path** — this app calls `send_hid_report` in-process instead of over the reference socket | **OPEN** — found 2026-09-09, previously untracked; scope corrected 2026-09-09 | Reference **does** implement it: `fn start_input_listener` (`ccpa/carplayd/src/main.rs`, binds `127.0.0.1:9110`; started from `main()` before the accept loop). Gap is this app only: the doc comment above `Java_zeno_gmccpa_pair_NativeCore_nativeTouch` in `native/carplay-jni/src/lib.rs` (which now cites `fn handle_input_frame`, `ccpa/carplayd/src/main.rs`; its earlier `airplayd main.rs:938` citation was stale) | No |
| N15 | Per-connection `build_info(&load_device_config())` not implemented **on the Kotlin path** — a static `/info` asset ships instead of a per-connection rebuild | **OPEN** — found 2026-09-09, previously untracked; scope corrected 2026-09-09 | Reference **does** implement it: `pub fn build_info` (`crates/vendor/receiver/src/info.rs`), `fn load_device_config` (`ccpa/carplayd/src/main.rs`) called per control connection in `run_pairing_server`. Gap is this app only: the `build_info(&load_device_config()) per connection` comment block in `JNI_OnLoad` (`native/carplay-jni/src/lib.rs`), which calls `receiver::info::build_info(&cfg)` once for its side effect | No |
| N16 | Three stale cross-references in one source comment — the `NOT YET IMPLEMENTED` block in `CarPlayRx.kt` (search `arm_keepalive`): (1) pointed at `01_FINDINGS.md` §8 for these gaps, but §8 is now "What to protect from the debloat" — correct tracking is N13–N15 here; (2) cited `main.rs:407-432` (no binary named) for `arm_keepalive` — actually `fn arm_keepalive` in `ccpa/carplayd/src/main.rs`; (3) cited `server.rs:119` for `av_idle_ms` — actually `ControlServer::av_idle_ms` in `crates/vendor/receiver/src/server.rs` | **LANDED** in `8cadba4` (2026-09-10) — found 2026-09-09 | Verified 2026-09-10: `CarPlayRx.kt` at `HEAD` carries none of the three stale strings, and all three replacements resolve — `fn arm_keepalive` exists in `ccpa/carplayd/src/main.rs` and is called in `run_pairing_server` right after `set_read_timeout(30 s)`; `ControlServer::av_idle_ms()` exists in `crates/vendor/receiver/src/server.rs` and is consumed by the `WouldBlock` arm of `serve_connection` in `net.rs`. **Not yet on the truck** — the installed APK is `7c92000`, which predates it | No |

### v5.8 / v5.9 landed (2026-10-08) — uncommitted working tree; "Device-verified" = cp12 bench (rc1/rc2) unless stated

| ID | What | State | Evidence | Device-verified |
|---|---|---|---|---|
| V1b/V2/V3 | Voice rebuilt real-time per stream: v2 tag, RTP jitter buffer (target `audioLatencyMs`, W), stock device buffers, NAV without focus, duck follows `duckAudio`/`unduckAudio`. **Supersedes A6, A7, A10 and A11's triggers below** (`keepAlive`, `sweepIdle`, `TRACK_BUFFER_MIN_MS`, the energy gate are deleted) | LANDED in tree | `av/VoiceJitterBuffer.kt`, `av/VoiceStreams.kt`, `av/VoiceLatency.kt`, `av/DuckRamp.kt`, `av/FocusSlot.kt` | Partly: nav live/no focus, platform duck, 0-loud-excess start (`04` §4e); B-V1-W matrix pending |
| fix2 | Start-stall exemption (allowance paid back from silence), `NAV_DUCK_LIFT_ON_BUS`, `VOICE_THREADS_URGENT_AUDIO` | LANDED in tree | `VoiceJitterBuffer.kt:220-262`, `VoiceStreams.kt:957-967`, `VoiceRouter.kt:421-428` | Lift and start observed on rc2; CPU effect of URGENT_AUDIO NOT measured (`perf_fable_findings.md` is provisional) |
| V5 / Q001 | Per-install pairing key + OD-8B automatic Forget Pairing (row T6.5 above) | LANDED in tree | `AccessoryKey.kt`, `RekeyFlow.kt` | Host JVM only; B-V5-1 pending |
| OD-7F | No new connection displaces an encrypted live session | LANDED in tree | `CarPlayRxSlots.kt:124-166` | Host JVM only |
| UI-1..3 | Recover -> Restart session; Box Info / Log Scope / Log Status removed; Adapter & logs sheet; Apply now; driver wording | LANDED in tree | `LauncherModel.kt`, `Ui.kt` | No — layout/keyboard pending |
| OD-7A | No compiled-in hotspot credentials; blank first run + dialog (closes R6.6.27's "rotate AP passphrase" as: owner chose no rotation, no scrub) | LANDED in tree | `Ui.kt:86-100`, `HotspotPrefs.kt`, `SessionCore.kt:765` | No |
| D6 / OD-9D / OD-9E | Auto-claim with the app closed (attach receiver, boot, media-browser bind), Stop lasts one boot, one status notification | LANDED in tree | `AutoClaim.kt`, `AutoClaimPolicy.kt`, `av/StatusNotice.kt`, `av/CarPlaySessionService.kt` (this refines row C3/T2.1: the service now also carries the adapter-link hold) | Media-browser-bind claim yes (rc2 04:43:27); other triggers, Stop-per-boot no |
| MED | Delta now-playing merge; idle-selectable media source; HIDDEN second launcher tile; OD-9C MainAudio take; focus reclaim on resume intent / screen return | LANDED in tree | `av/NowPlayingMerge.kt`, `av/CarPlayMediaBrowserService.kt`, `AndroidManifest.xml:221`, `av/MainAudioHandover.kt`, `av/FocusReclaimRule.kt` | One icon + metadata kept across same-artist skips on rc1 (session handoff, not re-checked here); take/reclaim pending (B-MED-9) |
| MIC | Bench-only `mic_inject` / `finish_launcher` behind `GMCCPA_BENCH=1` | LANDED in tree | `bench/BenchHooks.kt`, `nobench/BenchHooks.kt` | n/a (bench tool) |
| D016 | Kotlin pairing fallback removed | LANDED in tree | `CarPlayRx.kt:1359`; `pair/` holds only `NativeCore.kt` | n/a |
| Tier-0 | JVM tests in the gate (40 classes at v5.9; 46 after the 2026-10-08 remediation below), flavor switch, dirty-tree stamp | LANDED in tree | `tools/test.sh`, `tools/jvm_tests.txt`, `tools/build_apk.sh` | Gate runs host-side |

### 2026-10-08 remediation plan (Phases 1-7) — in the working tree on `02b12c0`; nothing bench-verified yet

Basis: six review passes (A-F) + a read-only bench-radio runtime review, every finding re-verified by three
independent agents; owner decisions D1-D9 recorded in the plan. "Device-verified" is **No** for every row — the
live owner session on the bench was not disturbed. Each row names its test where one is host-possible.

| ID | What | State | Evidence | Device-verified |
|---|---|---|---|---|
| E-F1 / owner rule | `tools/test.sh` built carplay-jni and the receiver/pairing `cargo test`s IN-TREE (≈1.7 GB under `~/Documents`); every cargo call now sets `CARGO_TARGET_DIR` under `~/.cache/cargo-targets/` (the JNI build shares `build_apk.sh`'s exact dir); new first gate step `tools/tree_artifact_guard.sh` FAILS on any real `target`/`build`/`.gradle`/`__pycache__`/`.build`/`node_modules` dir under `host/gm_ccpa` or `crates/` (symlinks pass; `GMCCPA_ALLOW_TREE_ARTIFACTS=1` only while cleaning up); `PYTHONDONTWRITEBYTECODE=1` + `python3 -I -B`; proto-check output under the run's mktemp dir | LANDED | `tools/test.sh`, `tools/tree_artifact_guard.sh`, `README.md` "Build and run", `06` §4 | n/a — the existing in-tree dirs are listed by the guard and deleted by the owner, not by the gate |
| E-F4 | `tools/adb_radio_probe.sh` wrote its capture beside itself (untracked → every later build `-dirty`); now `${GM_OUT:-/tmp}`; `__pycache__` and `radio_probe_*.txt` ignored (slashless) | LANDED | `tools/adb_radio_probe.sh:28-30`, `.gitignore` | n/a |
| E-F5 | `build_apk.sh`: `git rev-list --count` failure is FATAL (was `\|\| echo 7`); `cargo build --locked` (gate too); kotlinc version printed on drift (`KOTLINC_EXPECTED`, warn-only) | LANDED | `tools/build_apk.sh` | **Yes** — 2026-10-08 bench, build `230eaa7` (code 428), `install -r` over rc2: built and installed; versionCode 420 → 428 |
| B-F1 | `SessionSupervisor.onBoxLost` (non-GRACE) cancels the ladder's retry and resets `rung`/`skipped1`: a retry armed before the transport died fired into IDLE under the relink (rungs 1-2 skipped → false STALLED "wait a few seconds") or rung 2 `MGMT_RESTART_WIRELESS` 27 s into a successful relink; `onRelinkEnded` also rewrites a STALLED whose cause is ADAPTER | LANDED | `SessionSupervisor.kt` `onBoxLost`/`onRelinkEnded`/`stalledCause`; `SessionSupervisorTest.bf1_*` (5 tests, 4 of them red without the fix) | Partially — 2026-10-08 (evening) bench, build `6c95684` (code 430): adapter unplugged under a live session ("box gone, session live", CarPlay kept running), the phone-side break then ran the grace window and the ladder (rung 0, rungs 1-2 skipped `adapter: ABSENT`, STALLED with the accurate no-adapter text); after the replug (claim, HELLO_ACK 13 ms, SUBSCRIBE, armed) NO `MGMT_RESTART_WIRELESS` followed in the next 30+ s. Not the exact transport-death-mid-ladder case; keep Partially |
| C-F1 | Media focus REFUSED at start (AOSP rejects MUSIC while a CALL holds focus): media now HELD silent via the same `pausedForFocus` hold and re-requested every `FOCUS_RETRY_MS` while media flows (variant B, stock-like: CT5 Cinemo `CarPlayAudioManager.startAudio` configures streams only after a granted request); first refusal E, repeats I; KDocs corrected | LANDED | `av/AacPlayer.kt` `requestFocus` refusal branch, `FocusReclaimRule.shouldRequestFromFeed`; `FocusReclaimRuleTest.feedGate_*` | No — 2026-10-08 (evening) bench, build `6c95684` (code 430): a call active on the iPhone (Wi-Fi calling) through the whole sequence was routed as CarPlay's own telephony stream (SETUP audio type `telephony`, AAC-ELD 16 kHz) to the voice router, media was paused by the voice path, and media focus was GRANTED — the refusal branch never ran. The refusal case needs a call held on the HEAD UNIT's own Bluetooth phone |
| A-F1 | Unauthenticated connect to `:7011` put the supervisor in SESSION_UP at accept. Now: `sessionUp` + `supervisor.onSessionUp` move to the pair-verified callback (before the rekey early return); the pump closes a still-plaintext connection 10 s after accept, sending or silent, as `EndReason.UNVERIFIED`; SessionCore keeps that end from the supervisor (no GRACE/ladder reset). Screen + consumers still stand up at accept (option a) | LANDED | `UnverifiedConnRule.kt`, `CarPlayRx.handleNative`, `SessionCore.onPairVerified`/`onCarPlaySessionDown`; `UnverifiedConnRuleTest`, `SessionSupervisorTest.af1_*` | Partially — 2026-10-08 bench, build `230eaa7` (code 428), `install -r` over rc2: a normal (non-first) pairing ran receiver ready → armed → handoff sent → awaiting dial-back → pair-verify M1+M3 in 28 ms → live; the deadline did NOT cut it; first frame decoded ≈3.1 s after accept (ONE sample right after a reinstall, vs the 2.598 s baseline in `12` — neither a regression nor a pass; re-sample). 2026-10-08 (evening) bench, build `6c95684` (code 430): three further normal pairings (after an operator Stop/Start, after a force-stop relaunch, after a verb stop + launcher reopen) all completed pair-verify (41 ms in one), none cut by the deadline; first frame ≈3.0 s after pair-verify in one, ≈4.9 s from launcher reopen to CarPlay live in another. Still pending: a rogue non-local socket on the vehicle hotspot (needs a client on that network) and a first pairing after Forget (<10 s) |
| D-F1 | Notification and media-card `PendingIntent`s collided (same code 0, same component; extras ignored) so a card tap could auto-start over an on-screen Stop. Distinct request code AND action for the card (`TapTarget`) | LANDED | `av/TapTarget.kt`, `CarPlayMediaBrowserService.kt`, `CarPlaySessionService.kt`; `TapTargetTest` | No — Stop, then tap the media card: no claim (needs the owner at the bench) |
| D-F3 | `VehicleStateWatcher`'s three Car callbacks were field initialisers → `NoClassDefFoundError` at construction without `android.car`, outside `start()`'s try, before `rx.start()`. Now `by lazy` (built inside the try) and the construction is `runCatching` in SessionCore | LANDED | `av/VehicleStateWatcher.kt`, `SessionCore.startCarPlayRx`; `VehicleStateWatcherConstructionTest` (class-file field table) | n/a (truck has android.car) |
| B-F3 | Relink wait expired with the adapter enumerated-but-unlinked kept the LINK hold + "Adapter connected" notification forever; `onRelinkDone` now uses the C1 rule (`releaseLinkIfUnlinked`) | LANDED | `SessionCore.onRelinkDone`; `AutoClaimPolicyTest.releaseRuleAtRelinkEnd` | Partially — 2026-10-08 (evening) bench, build `6c95684` (code 430): with NO adapter on the bus a restart logged `relink: link NOT restored — no adapter claimed` then `link hold released (relink: no link)` — the release path, no-adapter case. The enumerated-but-ungranted ten-minute case is still pending |
| B-F2 | A Stop during the bring-up could not stop the CT_SUBSCRIBE radio-wake edge (one presence 0→1→0 cycle); `autoStart` passes `subscribe = { !stopRequested }`; operator Stop uses `stop(quick = !hasLink())` (no 4×6 s log pull on a dead link). Not done: interrupting the OcbmClient wait loops (76 s worst case past the claim wait) — `runAllLocked` is do-not-touch | PARTIAL (documented) | `SessionCoreParts.StartPolicy`; `SessionCorePartsTest.startPolicy_*` | No |
| D-F2 | A launcher recreate re-delivered the launch intent's `--es run` verb (`ocbm_forget`, `carplay_stop`, `export_log_raw` ran again). Verb runs only on a fresh delivery; the retained intent is stripped; `onNewIntent` → `setIntent`; auto-start still happens | LANDED | `MainActivity.dispatchIntent`, `SessionCoreParts.IntentReplay`; `SessionCorePartsTest.intentReplay_*` | No — rotate/recreate after a verb |
| D-F4 | Exported-verb gate | **CLOSED — owner decision D4: not coded.** Verbs stay open as today (the documented shell recipes in `06` §4/§8.2 depend on it). Next trigger: a non-bench user on the unit, or any verb that becomes reachable from another app | — | — |
| E-F2 | `MGMT_INFO` was logged in 110-char chunks, so a MAC split across lines escaped the line-wise export redactor; the adapter serial had no rule; the SESSION_DETAIL bond JSON is covered by the same rules. Chunks now break on JSON element boundaries (`MgmtInfo.logChunks`); Redactor gains SERIAL (`"serial":"…"`, `serial=`/`serial_number:`; verdict tokens exempt) and BOX_NAME (`CarLink-xxxx` is the Wi-Fi MAC's last two octets, ocbmd `bt_name_from`). Logcat itself stays unredacted by policy (`06` §8.2). Closes audit §0.3's serial item | LANDED | `logging/Redactor.kt`, `ocbm/MgmtInfo.kt`, `OcbmProbe.kt` MGMT_INFO line; `RedactorTest`, `MgmtInfoTest.logChunks_*` | Partially — 2026-10-08 bench, build `230eaa7` (code 428), `install -r` over rc2: whole-OS redacted export (12,260 lines, header scope WHOLE_OS with READ_LOGS granted): 0 full MACs, 0 literals of the vehicle-hotspot SSID/passphrase; other saved-network names (the phone's name and home SSIDs) DID leak through WifiConfigManager `configKey "<name>"` lines in the same export (rule added and tested the same day); placeholders MAC 27 / SSID 2 / IPv4 50 / IPv6 2 / LONG_DIGITS 233. The export held NO `MGMT_INFO` line, so the element-boundary chunking was not exercised on device. The new `configKey` rule (SSID category; security labels kept) is unit-tested only: `RedactorTest.configKey*` |
| E-F3 | `RE_WPA` missed `wpa_passphrase=`, `password=`, `hotspot_pass=`; widened with a lookbehind (keys reached through `_`/`-`); bare `pass` still deliberately excluded (KDoc corrected). Closes the redactor half of R6.6.29 (the box-side scrub stays OPEN there) | LANDED | `Redactor.kt` `RE_WPA`; `RedactorTest.passphraseSpellings` | Partially — 2026-10-08 bench, build `230eaa7` (code 428), `install -r` over rc2: 0 literals of the vehicle-hotspot passphrase in the export; whether any of the newly covered spellings occurred in the capture was not checked — unit-tested only |
| E-F9 | `start_epoch_ms=` lost to `RE_LONG_DIGITS` in every export; exempted by lookbehind, field format unchanged | LANDED | `Redactor.kt`; `RedactorTest.startEpochMsIsExemptFromLongDigits` | No — not exercised on device: the exported log held no `SESSION v=3` line (emitted at session end, and no session ended in the captured window); the lookbehind is unit-tested only. Next trigger: export after a session ends |
| E-F6 | Second export while the SAF picker was up overwrote the pending entry (first callback lost, wrong options); now refused with "export already pending" | LANDED | `logging/LogExport.trySaf` | No (Android-bound) |
| E-F7 | `LogFiles.open()` after `close()` could reopen a stream nobody closes; one-line guard | LANDED | `logging/LogFiles.kt` | n/a |
| E-DOC 12 | The printed `pm grant` recipe (degraded-scope WARN, capture header) carried no `--user`; now `grantCmdForUser` | LANDED | `logging/LogCapture.kt` | Partially — 2026-10-08 bench, build `230eaa7` (code 428), `install -r` over rc2: READ_LOGS was LOST on the `install -r` (the persistent USB grant survived) and `pm grant --user 10 … READ_LOGS` + force-stop (the `06` §8.1 recipe) restored whole-OS capture; whether the app's own printed line was the one read is not recorded |
| A-F2 / A-X1 | All Rust stdout/stderr reached logcat at INFO through one pipe. Kept ONE pipe (cross-stream order matters, and the receiver prints success on stderr); the JNI crate's own failure sites write a `[jni] E: `/`[jni] W: ` marker in one `write_all` (no interleave) and the reader maps it to ERROR/WARN (`log_prio`). Receiver-side severity: upstream | LANDED | `native/carplay-jni/src/lib.rs` `jni_log`/`log_prio`; `cargo test --no-default-features` `log_prio_maps_markers_only` | Partially — 2026-10-08 bench, build `230eaa7` (code 428), `install -r` over rc2: no JNI failure occurred, so no W/E line to observe; E-level lines from the app pid in the session = 5, all platform noise (jdwp agent, OpenGLRenderer ×3, ion ioctl); 0 EXPECTED-MISSING / EXPECTED-LATE; no crash or ANR |
| C-F7 | AUs the decoder never took (TRY_AGAIN, null buffer, oversize) were uncounted; `ausNotQueued` in the 500-frame and stop summaries, first at W | LANDED | `av/AacPlayer.kt` | **Yes** — 2026-10-08 bench, build `230eaa7` (code 428), `install -r` over rc2: the first 500-frame summary reported `1 AUs not queued, 3 underruns` (stream-start transient) — a drop that existed and was invisible before |
| A-X2 | `AccessoryKey.setRekey` published the flag without fsync; now fsync + `ATOMIC_MOVE` + dir fsync like the seed | LANDED | `AccessoryKey.kt`; `AccessoryKeyTest.rekeyFlag_publishedAtomically_noTempLeftBehind` | n/a |
| F-C8 | PHONE board read "DOWN (cleared by box — no phone identified)" under live CarPlay — expected in this role (the box never sees SETUP); text says so; `SessionSummary.onPhoneIdent` no longer erases a known identity with "" | LANDED | `ocbm/OcbmClient.kt`, `logging/SessionSummary.kt` | No |
| 4.5 | "SETUP negotiated … 0x0@0": `set_resolution(2400, 960, 60)` on the AvSession (log-only) | LANDED | `lib.rs` `nativeInit` | No — the line was not checked in this bench pass |
| D-F6 | Status notification said "Adapter connected" for FAILED-without-stall, RESTARTING, STOPPED and the initial null/IDLE; each has its own text now (closed set 7 → 11) | LANDED | `av/StatusNotice.kt`; `StatusNoticeTest` | No — text not reviewed on the unit in this bench pass |
| D-F7 | `MdnsResponder.start()`/`reannounce()` blocked their caller ~2-3 s (the receiver start, the OCBM claim behind it, the supervisor's scheduler on rung 0); announce runs on its own daemon thread, no sleep after the last packet | LANDED | `MdnsResponder.announceAsync`; `MdnsResponderStopTest.reannounceIsAsync…` | No — start-to-claim delta vs rc2 not measured (needs a like-for-like start) |
| D-F9 / D-F11 | `Ui.release()` dismisses the three dialogs; post guards also test `isDestroyed`; `CarPlayActivity.startSession`'s FGS start is `runCatching` — a refusal runs the session unheld (as `AutoClaim.acquireLink`), not a crash | LANDED | `Ui.kt`, `av/CarPlayActivity.kt` | No |
| F-C2 | GC churn: `HevcRenderer` allocated one `ByteArray` per AU and `AacPlayer` one per read and per decoded buffer. Grow-only scratch buffers; the NAL scan is a pure `(buf, len)` function so a reused buffer's stale tail can never read as a phantom parameter set | LANDED | `av/NalScan.kt`, `av/HevcRenderer.kt`, `av/AacPlayer.kt`; `NalScanTest.staleTailBeyondLenIsNeverScanned` | Indicative only — 2026-10-08 bench, build `230eaa7` (code 428), `install -r` over rc2, ≈3 min in: PSS 42.7 MB (rc2 39.2), RSS 174 MB (rc2 165-170), threads 72 (rc2 77), app CPU 26.6 % of a core (rc2 ≈32 %), logged ART GCs 10 in ≈3.5 min (rc2: 9 in 77 s). Different content, and ART logs only GCs over a threshold — NOT a controlled A/B |
| F-C3 | Launcher metrics applied inside the layout pass (requestLayout + button rebuild → second pass); now from an `OnPreDrawListener` | LANDED | `Ui.kt` | No — the launcher is covered while a session is live (D-F5), so gfxinfo/screenshot could not be taken |
| C-F2 | Lock-order inversion: `VoiceJitterBuffer.pull` ran `fateOf` under the jb monitor and that took `sinks` per node; `navSuppressed()` is now snapshotted once per pull pass outside the lock; the order comments corrected (the `identityLine` move was dropped on review) | LANDED | `av/PurposeEngine.kt` `fateOf`, `VoiceJitterBuffer.kt`/`VoiceStreams.kt` headers | n/a |
| A-F4 / C-F5 | Dead code: `toggleCarPlayRx`'s unreachable else (renamed `startCarPlayRx`); `Node.readUs` removed; `Node.removed` now enforced in `decoded`/`decodeLost` | LANDED | `SessionCore.kt`, `av/VoiceJitterBuffer.kt` | n/a |
| Phase 10 | Pure moves: `PurposeEngine` → `av/PurposeEngine.kt`, `OcbmBoard`/`OcbmExpect`/`logSourceName`/`PhoneIdent` → `ocbm/OcbmNames.kt` (prove with `git diff --color-moved`); six unused `now` params, one `!!`, one unused import; `ModesChanged.decode` extracted and run over the stored capture; **5.12 `/info`↔TXT equality test** — `InfoTxtEqualityTest` holds `CarPlayRx.txtRecordFor` against `assets/info.bplist` via `BPlist` | LANDED | `av/ModesChanged.kt`, `CarPlayRx.txtRecordFor`; `ModesChangedTest`, `InfoTxtEqualityTest` | n/a |
| Comments / docs | A-F3 (relay singleton → `ProcessMfiRelay`), B-F4 (MainActivity→SessionCore bindings, symlink, four→five methods, ocbm-proto cites), C-F3/C-F4/C-F6, D-DOC 1-5, E-DOC 1-12, `04`/`05`/`06`/`12`/`00` corrections | LANDED | this file, the cited files | n/a |

**Bench pass of 2026-10-08 on `230eaa7` — facts not tied to one row.** Install: `adb install --user 10 -g -r …`
over rc2 worked (versionCode 420 → 428); the persistent USB grant SURVIVED the upgrade (`UsbGrant status`
`is_granted=true`), READ_LOGS did NOT — it had to be re-granted with `pm grant --user 10 … READ_LOGS` plus the
force-stop (`06` §8.1): **a plain `-r` keeps the USB grant but not READ_LOGS.** The iPhone log showed 0 Identify
rejects (accessoryd/carkitd). Two startup warnings have no rc2 log to compare against and are recorded as
observations, not regressions: `ForceKeyFrame refused by the event channel — no IDR is coming` shortly after
pair-verify, and `BT phase went BACKWARDS WIFI_HANDOFF -> IDLE while the box reports PROJ_MODE NONE` during the
handoff. Export on this unit: `export_log` launches the SAF picker (DocumentsUI IS present here); the picker
opens in its own task and is covered by the CarPlay screen, which relaunches on MainActivity resume, so the
export stays pending until the app is brought forward — the cancel was processed ≈43 s later when MainActivity
resumed, then "falling back to rung 2+" landed the artifact in MediaStore Downloads (readable with
`content read --user 10 --uri content://media/external_primary/downloads/<id>`; `adb pull` of the Download
path is permission-denied for the shell). Observed limitation tied to D-F5 (the launcher is covered while a
session is live); no code change.

**Bench pass of 2026-10-08 (evening) on `6c95684` (code 430) — facts not tied to one row.** Measured from logcat
plus the iPhone syslog; times are the radio clock.
1. *Adapter unplug under a live session:* "box gone, session live" and CarPlay kept running without the adapter
   (usb-claim released). The phone-side break (19:52:33, control silent 6 s → `LINK_LOST`) stopped media focus
   and the voice router cleanly; the supervisor held 10 s, rung 0 re-announced, rungs 1-2 skipped with
   `adapter: ABSENT`, STALLED with "no CarPlay adapter on USB — check it is plugged in" (accurate). Replug at
   19:52:57: claim, HELLO_ACK 13 ms, SUBSCRIBE, armed; no `MGMT_RESTART_WIRELESS` in the following 30+ s.
2. *Phone-side recovery while the phone's Wi-Fi/BT were down:* every connect-out to the cached endpoint timed
   out (10 attempts, then "no usable response") and NsdManager resolves failed with `code=3`
   (ALREADY_ACTIVE) every ≈12 s from 19:54:18 until the operator Stop at 19:56:39; the first successful fresh
   resolve came at 19:57:19 after the phone rejoined (open row BP-1). Correction for anyone reading the iPhone
   log: `Bluetooth power state: ffffffff` is the NORMAL reply (it was also the value during the healthy 19:27
   session) and does NOT mean Bluetooth off; the real BT toggle shows as states `0` and `cafebabe`
   (19:54:15-52).
3. *Box reconnect:* in the bring-up after an operator Stop/Start (19:56:57) the box's "[reconnect] 1 bonded
   phone(s) — driving reconnect when idle" started 5 s after btd's HCI bring-up, paged the bonded phone (RFCOMM
   channel 1, MFi cert + sign ≈1.5 s, IdentifyAccept, 0x5702 → 0x5703), connect-out accepted on attempt 1,
   pair-verify OK, CarPlay live 19:57:29, media focus GRANTED, first frame ≈3.0 s after pair-verify. In the
   earlier bring-ups of the same window NO reconnect line was seen — root cause UNKNOWN, and it cannot be
   ordered from the box log because `[old]` lines are backfill replays stamped with the RELAY time (open row
   BP-2). In one bring-up `bt_on` set the BT name `ccpa-<4hex>` and `btd` then set `CarLink-<4hex>` — the
   divergence `docs/wireless/01_BT_AND_RADIO.md` §6c documents; the phone only ever sees the second name.
4. *Call test:* see the C-F1 row — the on-iPhone call reached us as the telephony stream; "assistant SPEAKING …
   media paused for the assistant" was logged for it while modes showed `phone=1` (open row BP-3).
5. *iPhone log:* the "Failed to parse accessory cert chain" errors at 19:55:35 belong to an audio accessory
   (processAudioProductCerts), not the box (box MFi auth: AuthSuccess); 0 Identify rejects in the capture.
6. *Force-stop:* after `am force-stop` (20:02:36) the system re-launched the app within ≈1 s through the
   media-browser bind ("session begun at link attempt (origin=launch)", "auto-claim (media-browser bind)"), the
   adapter was re-claimed, pair-verify at 20:02:40.4 and CarPlay live at 20:02:40.5 (≈4 s), the box paging the
   bonded phone first try. **A plain force-stop does NOT keep the app closed on this unit**; the in-app Stop
   does (until the next boot) — `06` §4.
7. *adb dropout:* the adb transport dropped at ≈20:02:43 for ≈13 min and came back as a new transport id; the
   radio did NOT reboot (uptime 2:19 at 20:16, boot reason `cold` = normal) and the app kept running (same pid,
   encrypted session, GAIN held). The "radio cold-restarted at 04:48:50 during rc2 testing" unknown in `00` may
   have been the same kind of adb dropout (unconfirmed). Cause unknown (no kernel/USB lines found).
8. *Stop/reopen by verbs* (`carplay_stop` + `ocbm_stop` at 20:18:56 — NOT the launcher Stop: no Stop-until-boot
   flag): the process stayed alive and nothing restarted it or re-claimed the adapter for the whole minute; adb
   stayed up. Launcher reopen ≈20:19:54 → receiver started, adapter claimed 20:19:55.9, SUBSCRIBE 20:19:57.8, box
   paged the phone 20:19:58.3, connect-out accepted on attempt 1 (0.7 s), pair-verify OK 41 ms, CarPlay live
   20:19:59.2 (≈4.9 s from reopen), media focus granted; CarPlay on screen confirmed by the owner. The
   `ForceKeyFrame refused by the event channel — no IDR is coming` warning appeared right after the renderer
   attached at EVERY session start on this build (no rc2 log to compare; open row BP-4).

Still pending, needing the owner's hands: a first pairing after Forget (<10 s deadline), a call held on the head
unit's own Bluetooth phone (C-F1), the launcher Stop button then a media-card tap (D-F1), a non-local rogue
socket on the vehicle hotspot (A-F1), the 10-minute ungranted relink (B-F3), launcher gfxinfo/screenshot (F-C3;
the launcher is covered while a session is live), and an export taken after a session has ended (E-F9).

#### Opened by the 2026-10-08 evening bench pass (OPEN; not coded)

| ID | What | State | Evidence | Device-verified |
|---|---|---|---|---|
| BP-1 | While the phone is away, the app dials a STALE cached `_carplay-ctrl` endpoint (10 attempts, "no usable response") and NsdManager resolves fail with `code=3` (ALREADY_ACTIVE) every ≈12 s until a Stop; the first fresh resolve succeeded only after the phone rejoined | **OPEN** | logcat 19:54:18-19:56:39 on `6c95684`; `CarPlayRx.rediscoverLoop` / `connectOut` | Yes — observed |
| BP-2 | Box-log relay (`CH_LOG`) replays backfilled `[old]` lines stamped with the RELAY time, not the box event time, so box events that preceded the link cannot be ordered against the app's. Fix is to carry the box timestamp (box side: static code — record; app side: print it if the frame ever carries one) | **OPEN** | the 19:56 bring-up analysis above | Yes — observed |
| BP-3 | A phone call routed as CarPlay's telephony stream is logged as "assistant SPEAKING … media paused for the assistant" (modes showed `phone=1`): check whether the label should distinguish a phone call from Siri | **OPEN** | logcat on `6c95684`, call test | Yes — observed |
| BP-4 | `ForceKeyFrame refused by the event channel — no IDR is coming` right after the renderer attaches, at every session start on this build; confirm whether it can ever leave the screen black (it did not in these sessions) | **OPEN** | logcat on `6c95684`, every session start | Observed; effect unconfirmed |

#### Closed, owner-tracked (not coded, with the trigger that reopens each) — D8/D9, 2026-10-08

| ID | What | Why not coded | Reopen when |
|---|---|---|---|
| F-C1 | Box `wireless` daemon's 1 Hz `EADDRINUSE` retry loop (SDP/HSP/HFP) = 32-37 % of the app's log lines (`crates/vendor/wireless/src/main.rs` retry loops, no streak dedupe) | Static box code (`crates/`); the holder of PSM 1 / RFCOMM 5-6 was never identified (needs a box console on the bench) | Owner opens a box console (CH_CONSOLE `ss`/`fuser`); then an upstream log-once-per-streak |
| 8.2 | Pair-setup fixed code `3939` (`lib.rs` F141): any hotspot client can reach ENCRYPTED and lock the real phone out via OD-7F `REFUSE_SESSION_LIVE` | Design/upstream (the code is what iOS CarPlay pair-setup expects; a per-install code is a receiver-crate change) | A hostile client is ever observed on the vehicle hotspot, or the receiver crate gains a setup-code setter |
| 8.3 | No source timestamps over the seam (`session.rs` forwards Annex-B only) and `/feedback` answered with an empty 200 (`server.rs`) — the iPhone never gets rfps/ltnc stats; candidate levers for the ~30 fps cadence | Static receiver code | Owner reopens Phase 9 (FPS) with iPhone-side evidence (`idevicesyslog -p AirPlaySender`) |
| A-F6 / T5.1 | `std::env::set_var` in `JNI_OnLoad` after threads may exist (`lib.rs:395-397`) | The fix is the T5.1 config refactor (`nativeInit` config), already an OPEN row above | T5.1 |
| B-F5 | `host/CarlinkAndroid`'s `OcbmProto.Mfi.parse` lacks the bounds guard this app's copy has (`if (len > payload.size) return null`) | Other project; latent, harmless hygiene | Next CarlinkAndroid session |
| DO LATER | `handleCtrl` arm split, `PumpIdleRule`, `DialPlan`, `DnsWire` golden vectors, `HeartbeatPolicy`, `OcbmProbe` transport injection | Refactors that need the adapter on the bench and rc3+ truck-accepted first, one commit each, test-first | After the rc3/rc4 bench checks above |
| 6.3 / F-C4 | Render pacing (`releaseOutputBuffer(idx, renderTimeNs)` on a smoothed timeline): the 1-/3-vsync bimodal cadence | Owner decision D6: optional, not adopted (≈ one frame of added latency; touch latency risk); cause recorded in `12` Phase 7. Re-measured on `230eaa7` 2026-10-08: 33 fps decode, SurfaceFlinger present avg 34.9 ms (28.6 fps), 1/2/3/4+-vsync = 53/13/55/5 — the same bimodal cadence as rc2, unchanged by the remediation (as expected: nothing in it paces frames). Owner accepted ~30 fps: Apple Maps navigation is largely static content and iOS adapts its encode rate to content/bandwidth, rising toward the advertised 60 only during animations | Owner opts in; measure with `SurfaceFlinger --latency` before/after |
| D-F4 | Exported-verb gate (`debug.gmccpa.*` property) | Owner decision D4 | See the D-F4 row above |
| F-C5 | Platform `ethctrlmgr` ~1.2k lines/s floods the 1 MiB main ring; the app's logcat child ≈3.4 % CPU | Platform; `persist.logd.size` is a device-owner action (`06` §8.4 record) | Never by the app; owner may raise the ring size |
| D9 | **Proposal, UPSTREAM:** let the app push a `bt_name` (e.g. follow the vehicle hotspot SSID) so the adapter's Bluetooth name is not `CarLink-<Wi-Fi-MAC suffix>`. Needs `ocbmd` (`bt_name_from`), `btd`/`bt_bringup` and `bt_on.sh` — static box code | Costs the owner weighed: a rename invalidates the phone's bonded record (forced re-pair); two adapters sharing one SSID-derived name would collide; the SSID would be broadcast over BT to every scanner. `docs/wireless/01_BT_AND_RADIO.md`'s "one box, one name" intent is the stock box-AP role's — in this app the AP is the vehicle's | Owner asks for it; then a `SubscribeConfig` key plus the three box-side readers |

### Audio (A6–A9) — filed 2026-09-10 from the 2026-09-09 capture — **A6, A7, A10 and the trigger half of A11 are SUPERSEDED by v5.8 V1b/V3 (table above); rows kept as history**

All four are **measured on hardware**, not derived. Owning document:
[`13_AUDIO_ROUTING.md`](13_AUDIO_ROUTING.md). The firmware facts they rest on were read from the
unit's own `/vendor/etc/audio_policy_configuration.xml` (cleanroom image of `W231E-Y181.3.2`), not
assumed. None is in the installed `7c92000` build.

| ID | What | State | Evidence | Device-verified |
|---|---|---|---|---|
| A6 | **`VoiceRouter` drops 17 % of Siri audio because `keepAlive` DOUBLE-FEEDS the track.** *(Diagnosis corrected 2026-09-10 — the first reading blamed the `w == 0` branch in `Sink.feed`; that is the symptom.)* `keepAlive` gates on `lastAudioAt`, which `feed` advances **only on `peakExceeds`** — i.e. on the last LOUD frame. But iOS streams silent AUs at realtime straight through the prompt/answer gap, so for the 3 s the driver is speaking the sweeper writes 200 ms of silence per 200 ms **on top of** iOS's 30 ms per 30 ms. 2× realtime into a 400–640 ms buffer fills it in ~0.5 s, and every AU write thereafter gets `w == 0` | **IMPLEMENTED in the working tree 2026-09-10; NOT on the truck** | Arithmetic from the capture: 249 AUs × 30 ms (ELD-480 @ 16 k) = **7.47 s**, and the sink lived 22:55:39.794 → 47.013 = **7.22 s** — so AUs arrived continuously for the whole turn, gap included. No `AU > input buffer` warning appears, so all 42 drops are the full-buffer branch. Audible symptom: Siri's answer starts up to a buffer late behind queued silence, first AU clipped. Fix is a second clock — `lastAuAt` (last **delivered** AU) gates `keepAlive`; `lastAudioAt` (last **loud** frame) keeps gating duck/idle/assistant — after which a blocking write is correct. **Also latent and made live by the same overlap: `keepAlive` (thread `cp-voice-sweep`) and `feed` (consume thread) call `AudioTrack.write` on one track with no lock.** Needs a `writeLock` | Defect confirmed on hardware. **Fix landed in the working tree:** `Sink.lastAuAt` gates `keepAlive`'s lower bound while `lastAudioAt` keeps gating duck/idle/assistant; the write loop is now `WRITE_BLOCKING`; a per-`Sink` `ReentrantLock` serialises `feed`/`keepAlive`/`release`, with `release()` calling `pause()` *before* taking it so a consume thread parked in a blocking write is interrupted rather than waited on — which also closes the pre-existing window where the sweeper released a codec under a live `getOutputBuffer`. `w == 0` is now classified paused-vs-full, so `ausDropped` means only "lost on a PLAYING track" and an incoming call mid-turn cannot masquerade as A6 |
| A7 | **The energy-gated duck pumps — but it is INAUDIBLE for ASSISTANT/CALL, and it exposes a real ordering bug.** `DUCK_RELEASE_MS` (1500 ms) against `sweepIdle`'s 1 Hz throttle un-ducks after ~1.5–2.5 s of quiet, shorter than Siri's own pause | **IMPLEMENTED in the working tree 2026-09-10; NOT on the truck** | All four `setVolume` edges landed on a track that was **paused** from 22:55:39.579 to 52.295, and `setVolume` on a paused track is inaudible. The bug worth fixing is elsewhere in the same method: `sweepIdle` **releases** dead sinks — which abandons focus, so AAOS posts `AUDIOFOCUS_GAIN` and `AacPlayer` calls `play()` — *before* it evaluates `if (!anyLoud) onDuck(false)`. Tighten ASSISTANT's hold to 4 s against an `idleMs` of 5 s and a late sweep lands both edges in one pass: **media resumes at 0.2**. Fix: un-duck before release, and give `Purpose` a per-purpose `duckHoldMs`. The case that actually matters is **NAV**, not Siri — if AAOS delivers `CAN_DUCK` there, the focus duck masks the energy gate and holds media at 0.2 until the NAV sink releases at `idleMs` = 8 s. Measure that | **My premise was partly wrong, corrected 2026-09-10 by the implementing pass:** with the constants as they stood the two edges could NOT land in one sweep (un-duck at quiet ≥ 1.5 s, release at > 5 s, 1 Hz sweeps). The ordering bug becomes reachable only *because* the same change adds the per-purpose `duckHoldMs` (ASSISTANT on `ASSISTANT_HOLD_MS`), which is what makes "one duck, one restore per turn" verifiable. Both landed together: `duckHoldMs` folds into the single shared `anyLoud`, and `sweepIdle` now computes `anyLoud`/`dead` under the `sinks` lock, sends `onDuck(false)` outside it, then re-checks and releases. NAV behaviour still unmeasured |
| A8 | **`AacPlayer` media buffer is rate-blind and smaller than one delivery pause.** `buildTrack` uses `maxOf(getMinBufferSize(), 8192) * 2` = 85–160 ms at 48 k stereo; the phone pauses ~200 ms between bursts | **IMPLEMENTED in the working tree 2026-09-10; NOT on the truck** | 6 underruns in 843 frames, with **no DEEP_BUFFER mixPort** to fall back on. AudioFlinger will not mix a fresh `MODE_STREAM` track until its buffer has filled once, so the buffer depth **is** the initial lead — 85–160 ms hits zero on the first pause, which is why 5 of the 6 underruns are in the first 9 s. Proposed 750 ms (3.75× the pause; matches `carlink_native_personal`'s media split and ExoPlayer's `DefaultAudioSink` ceiling). **Blocked on a counter fix first:** `AudioTrack.write` becomes non-blocking on a PAUSED track and returns short, and `AacPlayer.feed` checks only `w < 0`, so ~170 frames (3.6 s) were discarded while paused and counted as played in this very capture. The before/after underrun numbers are not comparable until that is fixed | Defect confirmed; baseline contaminated. **Both fixed:** `TRACK_BUFFER_MS = 750L` with a `maxOf(minBuf * 2, …)` floor, and `feed` now classifies every output buffer as PLAYED / DISCARDED_PAUSED / NO_TRACK / NO_PCM so exactly one counter moves. The over-count was **~240 frames (~5 s), not the ~170 first estimated** — iOS ended stream 102 17 ms after the focus pause and restarted it 5.1 s before the track resumed. `ERROR_DEAD_OBJECT` and other negative returns were previously counted as *played* too; they are now NO_TRACK |
| A9 | ~~Build the voice sinks at 48 kHz stereo~~ — **WRONG, closed 2026-09-10, no work to do** | **CLOSED — refuted** | `MediaCodec`'s AAC decoder has no output-rate control, and an `AudioTrack`'s declared format IS the format of the bytes written to it — writing 16 k mono PCM into a 48 k stereo track plays it 6× fast and channel-garbled. The only client-side rate knob is `PlaybackParams`, which is AudioFlinger's per-track resampler: **the same code path a 16 k track already uses.** So AudioFlinger resamples in every viable design and the change moves nothing; doing it properly would mean re-implementing a polyphase FIR in Kotlin on the consume thread. Two further refutations of the premise: `getMinBufferSize` already scales by the output-thread rate ratio (`AudioTrack::getMinFrameCount`), so it is **not** "sizing against a rate the HAL never serves"; and `keepAlive`'s `rate * KEEPALIVE_PERIOD_MS / 1000` byte maths is in codec terms and would under-write by 3× on a 48 k track. Measured cost of the status quo: one mixer resampler per active voice track, well under 1 % of an Atom core — unmeasurable against a software AAC-ELD decode. **The real win in this neighbourhood is `TRACK_BUFFER_MIN_MS = 400`**: by the pre-fill rule Siri's first word cannot sound until 400 ms of PCM is written. Re-filed as A10 | n/a |
| A10 | **Siri's first word is delayed by the voice track's 400 ms pre-fill.** `TRACK_BUFFER_MIN_MS = 2 * KEEPALIVE_PERIOD_MS` floors every voice track at 400 ms, and AudioFlinger will not mix until that has been written — 13 AUs at realtime | **OPEN — floor deliberately NOT changed; instrumentation added instead** | Split out of A9. **My proposed target of ~250 ms was refuted by the implementing pass and the floor was left at 400 ms.** Reason: once writes block, the track sits near full in steady state, and when the stream stops the first silence write cannot land until `KEEPALIVE_AFTER_MS` (250 ms) later — so the underrun margin is `floor − KEEPALIVE_AFTER_MS`. At 400 ms that is 150 ms against a sweeper that can be a 100 ms tick late; at 250 ms it is ~0. The correct change lowers `KEEPALIVE_AFTER_MS` and the floor **together**, after measurement. Instrumentation landed for exactly that: `[voice] siri: FIRST AUDIO OUT +Nms after configure (track buffer 400ms, K AUs written)` | Not measured — the measurement is now in the build |

| A11 | **The whole voice-state machine infers what iOS already TELLS us, on a channel we already receive and already throw away.** `VoiceRouter` decides "Siri is speaking" from an int16 peak threshold plus `ASSISTANT_HOLD_MS`, and "Siri is done" from a 4 s quiet timer — while iOS is sending explicit `modesChanged` state on the encrypted control channel throughout | **IMPLEMENTED in the working tree 2026-09-10; NOT on the truck.** Supersedes the trigger half of A7 and A10 | **6-for-6 / 14-for-14 bracket in the 2026-09-09 capture.** The `modesChanged` plist changes size 287 B → **277 B** at 22:55:39.165 and back to 287 B at 47.078. All six 277 B messages fall inside the Siri turn; all fourteen 287 B ones fall outside it. The mic gate ran 39.482 → 47.013, so **iOS signalled Siri starting 317 ms BEFORE the gate opened and signalled it ending 3.27 s before our `assistant done` inference and 5.2 s before media actually resumed.** The payload already reaches Kotlin: `session.rs` forwards it to `:9004` as `META_CMD`, `MetadataSeam` hands it to `CarPlayActivity.onCommandPlist` — which returns early unless `type == "requestViewArea"` and whose own KDoc names `modesChanged` and **`duckAudio`** as deliberately ignored. Two more explicit signals are also already arriving and unused: **`0x4155` CallStateUpdate** (subscribed at session start, first reply logged 22:55:24.202) for call start/end, and NowPlaying **`playbackStatus`** (0 stop / 1 play / 2 pause / 3 seek-fwd / 4 seek-back), which `NowPlayingState` already parses for the media card while `AacPlayer` infers play/pause from bytes on the seam instead. **The caveat is discharged: this is a parse now.** A capture already in this repo —
`docs/ops/captures/2026-07-24_carplay_cmd_capture.bin`, 35 `modesChanged` frames — decodes to
`params.appStates[]{appStateID, entity, speechMode}` plus `resources[]`. Apple's writer uniques
integers, and `speechMode: -1` is the **only** 8-byte int in the frame (marker `0x13` + int64 = 9 B
plus its 1 B offset entry), so `-1 → {0,1,2,3}` is exactly **−10 B**. Rivals enumerated and excluded:
drop the key −24, drop the Speech state −31, drop a resource −38, change an `entity` 0, a
non-uniqued value −7. Re-serialising the captured frame reproduced the delta. `appStateID 1` is
provably Speech (the only state carrying `speechMode`); 2/3 are believed PhoneCall/TurnByTurn from
`AirPlayCommon.h` and are logged, never acted on.

Implemented in Kotlin only, no Rust change: `CarPlayActivity.onModesChanged` decodes on `cp-meta`
(volatile writes only), `VoiceRouter.onModes` takes it, edges resolve on `cp-voice-sweep` — which
preserves A7's un-duck-before-release and e49492b's `Sink.lock` releaser discipline.
`CTRL_LEAD_MS` 3000 / `CTRL_AU_LIVENESS_MS` 1000 / `CTRL_END_GRACE_MS` 750.

**The energy gate REMAINS, mandatorily.** `iap2_core::metadata::emit_command_plist` `try_lock`s and
DROPS the plist under contention — against NowPlaying at ~2/s and up to 8 MiB of artwork on the same
`SINK` mutex — so a lost edge is expected, not exceptional. Six failure cases are handled by
construction and "phone never sends modes" is byte-identical to today.

Two things deliberately NOT done: **`duckAudio` is not a duck trigger** (zero arrived this session;
the duck stays energy-gated because it is the only per-prompt signal NAV and ALERT have) —
**CORRECTED 2026-10-07 (plan v5.8 V3): duckAudio arrives at every nav prompt (cp7, cp8: 51 of 57
SETUP-101s), and since v5.8 the media duck follows `duckAudio` / `unduckAudio` with their `volume`
(dB) and `durationMs` (stock R14 AirPlayReceiverPOSIX.c:387-418; bench T1/T5: 0.2 for nav speech,
0.1 for an SMS alert), through one VolumeShaper; the loud-frame energy duck is removed, an unduck
ramps media back to 1.0 at once as Cinemo does (the GM platform duck, -13.6 dB while bus1 is active,
carries the tail — stock T1; a software hold is the non-default switch NAV_UNDUCK_HOLD, decided by
bench B-V3-2, corrected 2026-10-08), and a lost unduck is restored at the END of its stream;** and
**`playbackStatus` is not wired into `AacPlayer`** (it is the Now Playing *app's* self-report — iOS
tore down stream 102 during Siri while Music presumably still read "playing", and a stale "playing"
would pin a dead track active, which is the exact stuck-knob bug the idle windows exist to fix).

Trade-off stated rather than buried: `CTRL_END_GRACE_MS = 750` gives up the AAOS volume-key
rationale in `ASSISTANT_HOLD_MS`'s KDoc — the knob re-latches to media ~0.8 s after Siri's last word
instead of ~5 s. Justification: iOS re-SETUPs media 93 ms after the END edge, so a native head unit
resumes immediately and the 4 s only ever covered our own inference latency. `3_000L` restores the
old behaviour at 2.25 s cost. A self-review during implementation also caught and fixed a
flag-before-stamp ordering bug that would have let one sweep pair a fresh END flag with a
seconds-old START stamp and cut the tail off Siri's answer. | Parse verified against a stored capture, and `BPlist.parse` verified by RUNNING it rather than by inspection: all 35 frames decode to a 3-element `List<Map<String, Any?>>` with `speechMode=-1L`, and frames mutated to `speechMode=1`/`2` decode correctly — it neither flattens nor fails closed, which was the one way this could have looked like it worked while doing nothing. Projected on the 2026-09-09 timeline: media resumes ~47.9 instead of 52.28, **~4.4 s of the 5.2 s recovered** |

**Verification rule for all of these: a green build is evidence of nothing.** Each needs a named log
line or measured number off the truck before it leaves OPEN — A6: `0 dropped` on a spoken Siri turn
plus a `silenceWrites` count that halves; A7: exactly one duck and one restore per turn, restore
logged *before* `siri: idle 5s — released`; A8: `0 underruns` after the counter fix, with
first-audio latency reported as the cost; A10: the measured pre-fill delay.

### Found by the 2026-09-09 capture (N17–N21)

| ID | What | State | Evidence | Device-verified |
|---|---|---|---|---|
| N17 | `ControlServer::av_idle_ms()` is not exported over JNI, so Kotlin's `handleNative` breaks the control connection on a 30 s read timeout even while A/V is flowing, where the reference `continue`s | **IMPLEMENTED 2026-10-06 (v5.5) — pending on-device acceptance.** Corrected in place 2026-10-06: this row said BLOCKED by the static-Rust constraint (2026-09-10), on the premise that the fix is an export from `crates/vendor/receiver`. That premise was wrong — `pub fn av_idle_ms` is already public there, so the export lives in this app's own `native/carplay-jni` and the crate is untouched | `Java_zeno_gmccpa_pair_NativeCore_nativeAvIdleMs` (`native/carplay-jni/src/lib.rs`) ↔ `NativeCore.nativeAvIdleMs(Long): Long`: `try_lock` on the core, so −1 = no A/V ever, −2 = unknown (core contended, generation mismatch, panic, or a stale `.so` caught as `UnsatisfiedLinkError`). The `handleNative` pump in `CarPlayRx.kt` `continue`s on its 30 s read backstop only while 0 ≤ av < 30 000 ms, otherwise breaks as `LINK_LOST` — the reference's `WouldBlock` arm of `serve_connection` (`net.rs`). The `NOT YET IMPLEMENTED` block that named this gap is removed | No — pending acceptance |
| N18 | **An `ocbmd` restart between the `CH_MFI` cert and sign leaves the replacement daemon with no host nonce for the rest of the session**, disabling the host-replacement detection the nonce exists for | **OPEN** | Device-observed: cert answered in 160 ms by `ocbmd` pid 127, the box supervisor then declared it wedged (`alive mtime stale >=1min`) and restarted it as pid 71, and the sign timed out at exactly 15 s. pid 71 served `CT_SUBSCRIBE` and all later `CH_MFI` ops **without a HELLO** — `ocbmd` gates neither on it — so `host_instance` stayed `None`. **IMPLEMENTED 2026-09-10** in `OcbmProbe.runAllLocked` with `REHELLO_TIMEOUT_MS = 4_000L`: on a sign timeout that follows a *good* cert, re-HELLO once and retry the sign once. Premise verified first — `Dispatcher::handle` in `ccpa/ocbmd/src/main.rs` routes `CH_MFI` and enters the `CT_SUBSCRIBE` arm with no check on `host_instance` or any hello flag. Safe because an L2 restart leaves the gadget CONFIGURED so the claim and read thread survive, `OcbmClient.hello()` is re-entrant, and the second HELLO carries the same process nonce so the box logs a reattach and touches neither presence nor `host_replaced`. Bounded cost: **+15 s worst case** on a real chip fault (healthy daemon, dead chip), **+4 s** on a genuinely wedged daemon. Note the box-side "wedged" verdict was itself a false positive against a healthy idle daemon — root cause is box-side and out of scope for this file | Defect confirmed on hardware |
| N19 | MFi **timeouts** are not recorded in `SessionSummary`; only non-OK statuses call `onMfiFailure`, so a timed-out op leaves `mfi=none` in the session line | **IMPLEMENTED 2026-09-10** | `OcbmProbe.runAllLocked`'s `cert == null` / `sig == null` branches logged but did not report; the 2026-09-09 session showed no MFi failure despite a 15 s sign timeout. Both now call `onMfiFailure`, and so do **all four `mfiRelay()` methods** — which had the same omission on the timeout path AND, in the two `Fast` iAP2 variants, on the `!ok` path too. Those are the timeouts that actually drop the phone, and none of them was being recorded. Relay failures are prefixed `relay ` so they read apart from the probe's own; a sign timeout recovered by the N18 re-HELLO records as `sign: timeout, recovered by re-HELLO`, so the restart stays countable | Defect confirmed |
| N20 | **No `SESSION v=1` line was emitted for a complete, successful session.** `SessionSummary.begin()` fires only from `UsbAttachActivity`, so any launch that is not a USB attach produces no summary at all | **IMPLEMENTED 2026-09-10 — schema bumped to v=2** | The whole 2026-09-09 capture contains zero `SESSION v=` and zero `SESSION_DETAIL` lines. Fixed by an `Origin` enum (`usb_attach` / `launch`), a nullable `hasPermissionAtTrampoline` rendering as `none`, and `origin=` **appended at the end** of the line per the file's own append-only contract. The launch-origin `AttachInfo` is built in one place, `SessionSummary.beginLaunch(dev)`, so the never-fabricate rule lives in one function: `perm_trampoline=none` and `serial=unknown` rather than plausible values, because `serial=sec_exception` is *defined* as "the attach-time grant did not land" and a launch-time read would mislabel itself. Called from `OcbmProbe.runAllLocked` before `awaitClaimable`, gated on `current() == null && findQuiet() != null` — before the wait so `prompted=` lands on the session, and only with the adapter present so a plug-in mid-wait lets the trampoline's real facts own it. `grep perm_trampoline=false` still matches only real trampoline observations. Two adjacent items folded in: the `awaitClaimable`/trampoline race that could drop `prompted=` (now an object-level `permissionDialogObserved()` with a 15 s pending-stamp window), and an honest `exit=launch_superseded_by_attach` label | Defect confirmed |
| N21 | `UsbAttachActivity` still banners `USB HANDLER (fixed-handler squat) … silent grant held=%b`. The squat was reverted 2026-09-08; both phrases now assert a mechanism that does not exist, and a reader of a fresh capture will infer it is still in place | **IMPLEMENTED 2026-09-10** | Verified 2026-09-10 that nothing greps `USB HANDLER` or `silent grant held` in tools, docs or `evidence/`, so the string is not load-bearing. The *fields* it carries (uid/user, process age, descriptor fingerprint) remain valid and are kept unchanged. Now banners `USB ATTACH TRAMPOLINE 0x%04x:0x%04x — permission held=%b, ccpa=%b`; the "Why this logs so much" KDoc moved to past tense with a dated note, and the same stale claim was fixed in `OcbmProbe.awaitClaimable` | n/a |

| N22 | Two pre-existing `AacPlayer` races the 2026-09-10 verification pass surfaced, both present in the **installed** build and untouched by the delta: (a) `requestFocus` writes `pausedForFocus = false` outside the `this` monitor and `reclaimFocus` then does an unsynchronized check-then-act on it, so a LOSS_TRANSIENT landing between the grant and the read can leave the track playing over the new holder with `pausedForFocus` true and nothing scheduled to re-evaluate; (b) neither `applyPauseState`'s play branch nor `feed`'s `reclaimFocus` call is gated on `running`, so a teardown racing either can leave a discarded player holding `AUDIOFOCUS_GAIN` with a live listener | **IMPLEMENTED in the working tree 2026-09-10; NOT on the truck** | Both are narrow windows and neither was observed in the capture. **Both confirmed and FIXED in the working tree 2026-09-10.** `requestFocus` now does all GRANTED bookkeeping in one `synchronized(this)` block and refuses under the monitor when `!running`; `stop()` writes `running=false` under the same monitor so the two are ordered by it rather than by racing volatile reads. The implementing pass also closed a mirror ordering I had not specified — a callback landing *before* the grant bookkeeping, which then clobbers it — with a request/callback sequence stamp, and made `abandonFocus` the owner of "no focus means no focus duck". **My related design note was WRONG and is retracted:** the consume thread does NOT park in the blocking write during a hold — native `AudioTrack::obtainBuffer` forces non-blocking whenever `mState != STATE_ACTIVE`, so `feed` keeps being re-entered and `reclaimFocus` *does* retry on its `FOCUS_RETRY_MS` cadence while the phone delivers. The real no-attempt case is the phone *ending the stream* (what it does for a Siri turn), after which the thread parks in the seam `read()` | No |

| N23 | **Probe scripts enumerate user 0 only.** `pm list packages` with no `--user`, in both `tools/adb_radio_probe.sh` and `tools/deepprobe.sh`. This app installs to user 10, so it is absent from its own probe, and the 2026-09-09 bundle makes several *kept* packages look uninstalled when the only thing established is that they are not on user 0 | **IMPLEMENTED 2026-09-10** | Both scripts gain `USERS="${USERS:-0 10}"` and an `sh_users` helper that runs the pm command once per user device-side, prefixes every line `[user N]`, and prints `[user N] (none on user N)` rather than a blank so an empty result is distinguishable from a failed one. Exercised against a fake `adb`. **NOT fixed, and it matters:** the 2026-09-09 bundle was NOT produced by either script — its recipe lives in `01_FINDINGS.md` — so whoever regenerates it needs the same `--user` fix or the next bundle lies identically | n/a |

| N24 | **The app narrated success and stayed silent on failure.** Measured on the 2026-09-09 reference session — a *successful* one: 641 app lines, **634 `I` / 7 `W` / 0 `E`**, containing a 15 s MFi sign timeout, 42 of 249 Siri AUs dropped and 6 media underruns. Three of the seven `W` lines were `*** MILESTONE` success markers, so `adb logcat -s NETPROBE:W` — which `ProbeLog`'s own KDoc advertised as how to pull problems out — returned nothing useful | **IMPLEMENTED 2026-09-10 in `6c6ec79`; NOT on the truck** | New `logging/SessionTrace.kt`: `!! EXPECTED-MISSING` (E) / `~~ EXPECTED-LATE` (W) / `== EXPECTED-MET` (I) and a `## STATUS` board of ~20 long-lived components. ~17 expectations armed, every budget from a measured number or an existing constant. Severity is now *judged* with tolerances, not blanket-promoted — underruns net of a baseline taken at first audio, one fill-timeout per stream start tolerated, `framesDiscardedPaused` never judged, MicUplink's first connect failure kept at `I` — because a false alarm is what gets a mechanism ignored. 12 silent error paths fixed, ~34 left as deliberate teardown paths with a `// deliberate:` note. **Filterability was the real gap:** `grep NETPROBE` misses the ~250 lines the app's PID emits under `CCodec`/`MediaCodec`/`BufferQueueProducer`/`ViewRootImpl`, which are exactly what distinguishes an app decode fault from a platform one — and the PID was printed only on the USB-attach path. `MainActivity` now emits `IDENTITY`/`PLATFORM` anchors every generation and `build_apk.sh` stamps `assets/build_sha`, since the SHA lived only in the APK filename and `pm install` discards it. Operator recipe in `06_BRINGUP_RUNBOOK.md` §8.3 | Two findings are EXPECTED to fire on the first run and are true positives: `av/voice-first-out/alert` (see `13_AUDIO_ROUTING.md`), and the READ_LOGS marker settling whether the fork explanation is even right |
| N25 | **The READ_LOGS "assigned at fork" explanation may be wrong**, and the degradation never recovered | **Self-heal IMPLEMENTED 2026-09-10; root cause OPEN** | In the reference session pid 3461 was forked **at boot** (22:51:14), capture started 9 s later reporting `read_logs=true`, and it still degraded to own-process — with no `pm grant` anywhere in that boot. So "the grant does not reach a process that was already running" cannot be the whole story on this unit. `ScopeMarker` now persists the grant observation plus the degraded pid and process start time in its own prefs file, so the next start re-requests WHOLE_OS and distinguishes same-process from fresh-process failure; a fresh-process failure now prints a *different* remediation. Related and unverified: `06_BRINGUP_RUNBOOK.md` §8.1 prints `pm grant` with **no `--user`** while the app installs to user 10 — `grantCmdForUser` now derives `--user` from the uid. Confirm on the next capture | Detection confirmed on hardware; cause not |

**Standing risk, not a row: the truck is running `7c92000` and HEAD is `3e37616`.** Everything landed
in `8cadba4` — the six code fixes, the debloat reconciliation, the comment and doc corrections — has
never executed on hardware. Three FABLE verification passes on 2026-09-10 found the `AacPlayer`,
`SessionSupervisor`/`OcbmProbe`/`OcbmClient` and `LogCapture` deltas all **correct and
behaviour-preserving**, so the gap is a deployment gap rather than a quality one — but no row in this
ledger may be moved to device-verified on the strength of a capture taken from the older build.

### Discovery / mDNS (R6.1–R6.2, 2026-08-27)

| ID | What | State | Evidence | Device-verified |
|---|---|---|---|---|
| N12 | `capturing requested=… effective=… read_logs=…` prints only resolved values, on the pump thread | LANDED | `logging/LogCapture.kt`: `val resolved: Boolean` on the status snapshot, and the pump's `log.i("capturing requested=…")` print | **Yes** (2026-08-27 deploy log) |
| A1 | Two-arg `joinGroup` at all three 5353 sites | LANDED | `MdnsResponder.start()`, `MdnsInspect.inspect()` and `MdnsInspect.liveEndpoint()` | **Yes** — br0 multicast users 1→2, `ENODEV` gone |
| — | mdnsd leg decision | RESOLVED — A1 alone was sufficient; speculative NsdManager re-registration dropped, not built | doc record only | **Yes** |
| RFC 6762 hygiene | AAAA answered with A + NSEC; announce spacing 250 ms → 1 s | LANDED | the AAAA/NSEC arm of `MdnsResponder.handleQuery()`, `recNsec()`, and the 1 s `Thread.sleep` in `announce()` | Partial (A1 verified; AAAA/NSEC path not separately isolated) |
| — | Capture always-on from `onCreate` | LANDED | (`LogCapture.start` wired) | **Yes** |

### Box side (`ccpa_custom`)

| ID | What | State | Evidence | Device-verified |
|---|---|---|---|---|
| Radio seam | `radio_hal.sh`/`radio_detect.sh` missing from deployed supervisor → no BT bring-up, no error surfaced | **LANDED (fixed 2026-08-28)** | `ccpa_custom docs/ops/06_CORRECTIONS_LEDGER.md` `R-20W-5`; `check_supervisor_deps()` in `tools/ocbm_push.sh` now warns on a partial push | **Yes** — `hci0 UP RUNNING` from cold boot after the two scripts were pushed |
| N3 | `pgrep -x`/`pkill -x ocbmd` (was `-f`, matched the respawn wrapper) | LANDED | `restart_ocbmd_daemon()` in `ccpa_custom tools/session_supervisor.sh` (stale line cite, re-anchored 2026-09-10 — the old lines are now the `carplayd` spawn in the ARM path) | Unverified this pass |
| N4 | Reboot budget write-verified after `sync`, fails closed | LANDED | the `FAIL CLOSED (audit N4)` block in `escalate()`, `session_supervisor.sh` (stale line cite, re-anchored 2026-09-10 — the old lines are now the `wireless_owns_session` ARM-suppression check) | Unverified this pass |
| 3.3 | `/tmp/bt_phase` un-latched (`rm -f` on exit/teardown/start) | LANDED | the `rm -f /tmp/bt_phase` in `wireless_down()`, `session_supervisor.sh` (stale line cite, re-anchored 2026-09-10 — the old line is now a comment block in `wireless_up()`) | Unverified this pass |
| 5.7 | BT SYN resend capped at 10 total | LANDED | `const SYN_RESEND_MAX` inside `fn session`, `crates/vendor/wireless/src/bt_driver.rs` (`SYN_RESEND_MAX = 9`, "audit 5.7") | Unverified this pass |
| 3.4/3.5 | Split HOST_GONE emission; `F_REPLAY` bit2 semantics | 3.5 **LANDED**; 3.4 **UNVERIFIED** (half) | Corrected 2026-10-08 (B-DOC): the source IS in this checkout. `F_REPLAY` is `crates/ocbm-proto/src/lib.rs:95` (bit2), set by ocbmd on every state-mirror replay (`ccpa/ocbmd/src/main.rs:1341-1348`) and consumed by `OcbmClient.kt:378` (`handleCtrl(…, replay)`). `SEV_HOST_GONE` is emitted from more than one site in ocbmd (`main.rs` ~`:1873` the unsubscribed-heartbeat nudge, ~`:3855` the answer path); whether that split matches the audit's intended semantics was not re-derived | No |
| R6.5 | TTL-gated deploy trial / dead-man before flashing a daemon | Process step, not code — treat as standing procedure, not a landed/open item | — | — |
| R6.6.27–28 | Rotate AP passphrase; rotate `edSeed`/setup code fallback | **OPEN** | Corrected 2026-09-09: the `crates/vendor/pairing/src/srp.rs` `b"3939"` citation was a false positive — every occurrence (`:205,218,226,231,232`) sits inside `#[cfg(test)] mod tests` (opens `:195`, EOF `:236`), a test fixture never read at runtime, not a shippable credential; dropped as evidence. The real setup-code fallback is `const PI_FALLBACK` in `ccpa/carplayd/src/main.rs` (read by the `CARPLAY_PI` `get_or_init` fallback) — the old `crates/vendor/rx-connect/src/main.rs:29` pointer was stale, that crate no longer exists in this checkout and appears to have been merged into `carplayd`. `edSeed` rotation remains open per T6.5 above | No |
| R6.6.29 | Scrub tracked credential files, widen `RE_WPA` redactor | scrub **OPEN**; widen **LANDED 2026-10-08** | `ccpa_custom/pi/evidence/hostapd_5g.conf` still tracked; no box-side `RE_WPA` symbol found in `ccpa_custom`. Scoping note (2026-09-09, cite corrected 2026-10-08): an `RE_WPA` symbol does exist, but in this repo as `RE_WPA` in `logging/Redactor.kt` (moved out of `LogExport.kt` in v5.8 V7) — that is app-side export redaction, not the box-side credential-file scrub this item is about, and must not be mistaken for coverage of it. The widening (E-F3: `wpa_passphrase`, `password`, `hotspot_pass`) is in the remediation table | No |

**On the two open box-side credential items (R6.6.27–29): these are real, unremediated exposures in
a sibling repo (`ccpa_custom`). This doc does not fix them (out of scope for this file) — flagging
here so the ledger doesn't imply otherwise. Do not print the actual passphrase/seed value into any
future doc; use a placeholder and rotate before any further sharing of that repo.**

---

## Open work, detail

Kept here because a one-line ledger entry isn't enough to act on.

- **T0.1 — pin `ccpa_custom`.** Still a bare relative path dep (the `receiver`/`pairing`/`mfi` `[dependencies]` entries in `native/carplay-jni/Cargo.toml`); the "LAST
  REVIEWED REV" comment is manually maintained, not enforced. Add either a submodule/vendored
  snapshot, or at minimum a build-time `git -C ../../../ccpa_custom rev-parse --short HEAD` compare
  against a `REVIEWED_REV` constant that fails the build (or stamps a loud banner) on mismatch.
  Until then, diff `ccpa_custom` by hand at the start of every session — see "Upstream drift" below.
- **T2.2 — full session-ownership relocation into `CarPlaySessionService`.** The service exists and
  owns process priority; whether `cpRx`/`ocbmProbe` construction itself now lives inside it
  (survives Activity *destruction*, not just backgrounding) was not confirmed this pass — read
  `CarPlaySessionService.kt` before assuming it's done. Land behind a `service.owner` flag if not:
  empty FGS → AacPlayer → OCBM → native core, A/V last (re-attach must preserve both the
  `@Volatile` renderer publish and the `:9001` socket close that forces producer re-dial).
  **Scope correction:** T2.2 as designed only ever covered `cpRx`/`ocbmProbe` ownership. It
  does NOT cover re-seeding the per-Activity observers on an Activity recreate. **Corrected 2026-10-08
  (D-DOC 4):** those observers are no longer Activity-owned — since v5.7 U4 `sessionUp`, the
  `VehicleStateWatcher` (one fresh instance per receiver start, `SessionCore.startCarPlayRx`) and the
  supervisor are all fields of the process-scoped `SessionCore`, so a MainActivity recreate loses none
  of them. What remains Activity-owned is the launcher view itself (`ViewSlot`, re-rendered on attach).
- **T5.1 — env-var config → `nativeInit` config.** Still the `−16720` desync class: `/info` is
  static-baked while `CARPLAY_SESSION_MGMT` etc. are env vars set in `JNI_OnLoad`
  (the `std::env::set_var(...)` block, `lib.rs`). Fix: one explicit config struct through `nativeInit`; generate `/info` in Rust
  from the same config; keep both paths with a startup byte-compare assert for ≥1 milestone before
  deleting the env path.
- **T6.4 — gate debug dumps.** `CARPLAY_SCREEN_DUMP`/`CARPLAY_SETUP_DUMP`/`.verbose(true)` are always
  on (the `CARPLAY_SCREEN_DUMP` `set_var` in `JNI_OnLoad`, `lib.rs`) → unbounded disk growth in production. Gate behind `BuildConfig.DEBUG`.
- **T6.5 — rotate `edSeed`. LANDED 2026-10-08 (v5.8 V5).** Was `(it*31+7).toByte()` — deterministic, derivable
  from source, identical across every install; the accessory's Ed25519 long-term key was not secret.
  Now generated once with `SecureRandom` per install (`AccessoryKey.kt`, `noBackupFilesDir`, not beside the
  peer store: that directory is backup-excluded and the peer store's `filesDir` is not). **Invalidates every
  existing pairing** — handled by the OD-8B automatic Forget Pairing + hint (see the table row). Bench
  confirmation (B-V5-1, iOS's response to the key change = U10) still pending.
- **M7/T5.4 — Rust owns the control listener.** Deferred by design: it replaces the one
  device-proven accept path and re-times the advertise-before-dial race. `receiver::net::serve_connection`
  is already shaped for it. Needs a re-authored ~100-line accept loop (not a port — `run_pairing_server`
  isn't in this checkout) plus a new upward `SessionListener` callback so Kotlin doesn't lose the
  accept event that rediscover depends on. Attempt only after everything above is truck-proven, one
  variable at a time.
- **5.12 — `/info`↔TXT build-time test.** No test asserts `info.bplist` against the Kotlin TXT
  constants; a desync still only fails on hardware as pair-verify `-16720`.
- **5.2 — per-scope sweep budget. LANDED.** `LogFiles.sweep()` (`logging/LogFiles.kt`) ages
  and ceilings each scope (`netprobe-own-` / `netprobe-os-`) independently under its own `Budget`,
  rather than applying one shared 64 MB ceiling across scopes — the earlier "shared ceiling" read of
  `LogCapture.kt` was stale; that file only lists files, it does not sweep.
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
   (`set_var("OCBM_FWD_ENC", "0")`, `lib.rs`) and `levers::fwd_enc()` treats `0`/`false`/`off`/empty as opt-out. gm_ccpa's
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

A deploy that pushed `session_supervisor.sh` (969→1227 lines) and `ocbmd`/`btd` binaries
left the box with `BOX_HEALTH 0x50` — `HCI_PRESENT` bit clear, no Bluetooth radio, no pairing
possible — while OCBM claim/HELLO/MFi/SUBSCRIBE all still reported success with no error anywhere.

**Root cause, confirmed on hardware:** `radio_hal.sh`/`radio_detect.sh` were absent from the deployed
unit (`ls` returned "No such file or directory"). The supervisor invokes them inside a detached
`setsid` wrapper and never reads the exit status, so the missing scripts produced no signal. Fix:
push the two scripts; `hci0` came up `UP RUNNING` from a cold boot. See `ccpa_custom docs/ops/06_CORRECTIONS_LEDGER.md`
`R-20W-5` for the full account. **Process fix landed:** `tools/ocbm_push.sh` now warns when a
supervisor push is missing the radio seam (`check_supervisor_deps()`, `tools/ocbm_push.sh`).

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
