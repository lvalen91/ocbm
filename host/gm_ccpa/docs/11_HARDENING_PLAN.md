# 11 — Hardening: status ledger and open work

The ledger below is the single source of truth for what is LANDED, OPEN, DEFERRED or device-verified. It
started from a 2026-08-09 code review (Appendix A) and covers the 2026-08-27/28 lifecycle work, the R6
post-audit remediation, v5.8/v5.9 and the 2026-10-08 remediation. Everything after the ledger is reference
and is subordinate to it. Evidence cells cite code by symbol; re-anchor by grep, not by line.

**Governing constraints**
- **`crates/` and `box/` Rust is static.** (Owner, 2026-09-10.) It is proven on the box and in the macOS host and
  this app is one of three consumers, so `gm_ccpa` adapts to it. Where the honest fix is upstream, record a
  follow-up and solve it in Kotlin or not at all (A11 landed Kotlin-only for this reason; the cheaper
  `speechMode` atomic + `nativeSpeechMode()` export was written down, not built). The one exception is `c39c9af`,
  which corrected four assertions inside `#[cfg(test)] mod tests` in `receiver`; no shipping binary differs.
- The doc-05/06 proven flow is the regression oracle for every change.
- Box rig state is reversible: track and restore the patched supervisor, `/etc/hostapd.conf` and
  `/tmp/no_escalate` (tmpfs, clears on reboot) before any truck test.
- GM `CarplayService` coexistence is proven (2026-09-09: full A/V with GM's service live and untouched); still log
  its state every session.

---

## Status ledger

State legend: **LANDED** (confirmed in source), **OPEN**, **DEFERRED** (deliberately not doing yet), **UNVERIFIED**
(could not confirm). "Device-verified" is a separate claim from LANDED: **Yes**, **Partially**, **No**, or n/a.

### App/session lifecycle (R1-R2, 2026-08-09 and 2026-08-27/28 work)

| ID | What | State | Evidence | Device-verified |
|---|---|---|---|---|
| C1/T1.1-1.3 | Generation-checked native handle registry; uniform `catch_unwind`; `feed` null-on-error | LANDED | `static CORE`, `fn core()` in `native/carplay-jni/src/lib.rs` | No |
| C2/T1.4-1.7 | Connection hijack, per-generation destroy, rediscover-on-session-end | LANDED | `CarPlayRx.acceptLoop`, `IncumbentSlot` (`CarPlayRxSlots.kt`) | No |
| — | `OcbmProbe.runAll()` returns `LinkResult`: launcher no longer claims "MFi proven" with no adapter | LANDED | `OcbmProbe.LinkResult` | No |
| — | `SessionHolder`: receiver/probe/sink process-scoped | LANDED | `SessionHolder` in `MainActivity.kt` | No |
| — | `NativeCore.BUSY` bounds `nativeInit` on CORE contention | LANDED | `pair/NativeCore.kt` | No |
| C3/T2.1 | Foreground service + manifest `<service>` | LANDED | `av/CarPlaySessionService.kt` (also carries the adapter-link hold, D6) | No |
| T2.2 | Session ownership into the service so it survives Activity destruction; covers `cpRx`/`ocbmProbe` only. The per-Activity observers (`sessionUp`, `VehicleStateWatcher`, supervisor) are already fields of process-scoped `SessionCore` (v5.7 U4); only the launcher view is Activity-owned | **OPEN** | `SessionCore` holds `cpRx`; the service holds the notice and link hold. If pursued: empty FGS -> `AacPlayer` -> OCBM -> native core, A/V last, behind a `service.owner` flag; re-attach must keep the `@Volatile` renderer publish and the `:9001` close that forces producer re-dial. Highest regression risk of the open items | No |
| T4.8 | Serialize the command dispatcher through one executor; guard destructive verbs | LANDED | `SessionCore.cmdExecutor` (single-thread, process-scoped, one per process: never shut down, F023) | No |
| — | `CarPlayActivity.onSessionEnded()` tears the screen down on session end | LANDED | `av/CarPlayActivity.kt` companion | Partially: it fixed a device-observed cold-start bug; teardown not re-driven alone |
| — | `AacPlayer.reclaimFocus()` recovers media after permanent `AUDIOFOCUS_LOSS` | LANDED | `av/AacPlayer.kt` | No |
| — | `AacPlayer` focus listener + duck/pause/restore (R6.7 item 31) | LANDED | `AacPlayer.requestFocus`, `focusListener` | No |
| C4/M3/T3.1-3.4 | `OcbmClient` `mfiLock`, length-correlated MFi responses, `Mfi.parse` truncation reject, `Tlv8` coalescing (`Tlv8` removed with the Kotlin pair-setup stub, D016) | LANDED | `ocbm/OcbmClient.kt`, `ocbm/OcbmProto.kt` | No |
| T5.3 | `mfi-i2c-local` behind `local-mfi` (off on Android); `iap_tunnel` through `Arc<Mutex<dyn MfiSigner>>` | LANDED | `receiver` feature `local-mfi`; `receiver::iap_tunnel::set_remote_signer` call in `nativeInit` (`lib.rs`) | **Yes, 2026-09-09**: real `0xAA01`/`0xAA03` round trips via the OCBM relay (945 B cert, 128 B sig), tunnel `Identified`, three metadata channels subscribed, zero `lock busy` (`12`) |
| — | Box-log streaming into logcat via `CH_LOG` push (`OcbmClient.handleLog`, armed by `OcbmProbe.startBoxLogStream()`); `CH_FILE` is only the end-of-session snapshot | LANDED | `OcbmProbe.captureBoxLogsNow()`, `OcbmClient.filePull()` | **Yes** |
| T5.1 | `JNI_OnLoad` env-var config -> one explicit `nativeInit` config; generate `/info` from the same config (the `-16720` desync class). Keep both paths with a startup byte-compare assert for >=1 milestone before deleting the env path | **OPEN** | the `std::env::set_var(...)` block in `JNI_OnLoad` (`lib.rs`) | No |
| M7/T5.4 | Rust owns the control `TcpListener`/accept loop | **DEFERRED** (hardware-gated) | no `TcpListener` in `lib.rs`. Needs a re-authored ~100-line accept loop (`run_pairing_server` in `box/carplayd/src/main.rs` is the reference, not a port) plus an upward `SessionListener` callback so Kotlin keeps the accept event rediscover depends on. Replaces the one device-proven accept path: attempt last, one variable at a time, keep the Kotlin listener runtime-selectable | No |
| T6.4 | Gate `CARPLAY_SCREEN_DUMP`/`CARPLAY_SETUP_DUMP`/`.verbose(true)` | **CLOSED 2026-09-10: premise wrong, do not gate** | `spawn_screen` in `crates/vendor/receiver/src/session.rs` guards the dump with `if screen_dump && frames < 6`: at most six lines per session, and those are the only in-band codec identity a capture carries (they proved `hvc1`). `.verbose(true)` logs control-plane lines only, including the `auth-setup (MFi-SAP) OK/FAILED` verdict. `CARPLAY_SETUP_DUMP` stays unset (unbounded per-SETUP files in an unreadable dir). Output is bounded to six lines per session | Bound verified: 6 lines in a 1358-frame session |
| T6.5 | Per-install `SecureRandom` `edSeed` (invalidates pairings) | **LANDED 2026-10-08 (v5.8 V5, Q001)** | `AccessoryKey.kt`: 32 bytes in `noBackupFilesDir/carplay_accessory_ed25519.seed`, created once under `O_EXCL`; `CarPlayRx.edSeed` has no default. A seed created over an existing `carplay_peers.bin`, or a corrupt/legacy file, sets a rekey flag: OD-8B runs Forget Pairing once on the next OCBM link and keeps a launcher hint until pair-verify succeeds. Logs `created`/`loaded`/one E, never key material. `AccessoryKeyTest` | Host JVM only; B-V5-1 pending |
| T0.1 | Pin `ocbm` (submodule/vendored snapshot) instead of bare path deps | **OPEN** | `receiver`/`pairing`/`mfi` entries in `native/carplay-jni/Cargo.toml` are `path = "../../../../crates/vendor/..."`; the "LAST REVIEWED REV" comment is manual. Fix options: a submodule/vendored snapshot, or a build-time compare against a `REVIEWED_REV` constant that fails or banners on mismatch. Residual hazard: the crates compile from the working tree, visible in the stamped SHA only as `-dirty` (build-time rev check is moot: one repo, `build_apk.sh` stamps the whole tree). See "Upstream drift" | No |
| T0.2/T0.3/T0.5 | R8 keep-rule for `MfiRelay`; `build_apk.sh` -> android-32, fail-fast, `pipefail`; `Cargo.toml` lto/strip/codegen-units | LANDED | `app/proguard-rules.pro`; `ANDJAR`/`CARJAR`, `set -euo pipefail` in `tools/build_apk.sh`; `[profile.release]` | No |
| T6.1 | Delete `AirPlayRx.kt` + wirings | LANDED | absent from tree | No |
| T6.3 | Delete dead code (`unsafe impl Send for Native`, `_UNUSED`, `nativeReady`) | LANDED | absent from `lib.rs` | No |
| 5.6 | `ACTION_CANCEL` emits MOVE-then-slop, not a phantom tap | LANDED | `av/PrimaryPointerFilter.kt` (`ACTION_CANCEL` arm, `PHASE_CANCEL`), consumed in `av/CarPlayActivity.kt` (`phase == PrimaryPointerFilter.PHASE_CANCEL`); `PrimaryPointerFilterTest` | No |
| 5.10 | `server` field `@Volatile`, bound before dispatch | LANDED | `@Volatile private var server`, `BIND HERE, NOT ON THE POOL THREAD` in `CarPlayRx.start()` | No |
| 4.2/N8 | Recovery ladder: cooling rung waits; `handoffTimer` cancelled in accept path | LANDED | `SessionSupervisor.escalate`, `onDialAccepted` | No |
| 4.2/N8b | Ladder stands its blind retry down on forward progress (`deferLadder`); a rung had torn down a healthy session twice, 2026-09-08 | LANDED 2026-09-08 | `SessionSupervisor.deferLadder`, `onBtPhase`, `onDialAccepted`; `04` "The recovery ladder" | **Not yet**: needs a rung firing while a handshake starts inside the retry window |
| Leak fixes | `OcbmProbe` executor shut down on teardown; command executor is NOT shut down | LANDED (probe); command executor process-scoped by design | `OcbmProbe.stop` calls `ops.shutdown()` after the teardown task (so the `ocbm-session` thread does not survive Stop -> Start). `SessionCore.cmdExecutor` is one per process and never shut down (F023) | No |
| 5.12 | Build-time test asserting `/info`↔TXT equality | **LANDED 2026-10-08** | `app/src/test/java/zeno/gmccpa/InfoTxtEqualityTest.kt` (`CarPlayRx.txtRecordFor` vs `assets/info.bplist`) | n/a (host test) |
| 5.2 | Per-scope (not shared) sweep budget in `LogCapture` | LANDED | `LogFiles.sweep()` (`logging/LogFiles.kt`) | No |
| N6 | Drop `CORE` lock across the blocking JNI upcall | **OPEN**: `deep` tier, own commit. The held-lock structure is real; the motivating "10 s phone timeout" figure is unsourced | not attempted | No |
| HEVC split (T4.5 residual) | Separate read/decode threads behind `hevc.reader.thread`; a >2 s decode stall still trips the producer's write timeout. Design: reader thread + bounded queue, drop-oldest whole AUs, never a keyframe/param-set AU | **OPEN** | no flag in `av/HevcRenderer.kt` | No |
| N13 | TCP keepalive on the Kotlin path (`java.net.Socket` cannot set idle/interval/count); reference arms 3/3/3, ~12 s dead-link detect | **IMPLEMENTED 2026-10-06 (v5.5)** | `CarPlayRx.armKeepalive` per accepted control connection in `handleNative`: `SO_KEEPALIVE`, `TCP_KEEPIDLE/INTVL/CNT` = 3/3/3, `TCP_USER_TIMEOUT` 12 000 ms via `Os.setsockoptInt` on a `ParcelFileDescriptor.fromSocket` dup. Reference: `fn arm_keepalive` in `box/carplayd/src/main.rs` | No: pending acceptance |
| N14 | `start_input_listener()` HID endpoint on `127.0.0.1:9110` not implemented on the Kotlin path (the app calls `send_hid_report` in-process) | **OPEN** | reference: `fn start_input_listener`, `fn handle_input_frame` (`box/carplayd/src/main.rs`); gap: doc comment above `nativeTouch` in `lib.rs` | No |
| N15 | Per-connection `build_info(&load_device_config())` not implemented on the Kotlin path (static `/info` asset) | **OPEN** | reference: `pub fn build_info` (`crates/vendor/receiver/src/info.rs`), `fn load_device_config` called per connection in `run_pairing_server`; gap: comment block in `JNI_OnLoad` calling `build_info(&cfg)` once for its side effect | No |
| N16 | Three stale cross-references in the `NOT YET IMPLEMENTED` block in `CarPlayRx.kt` | **LANDED** in `8cadba4` | block removed; `fn arm_keepalive` and `ControlServer::av_idle_ms` resolve in `box/carplayd/src/main.rs` / `crates/vendor/receiver/src/server.rs` | No |

### v5.8 / v5.9 (committed 2026-10-08: `02b12c0`; remediation `b392393`..`6c95684`, ledger `f5c7d87`)

"Device-verified" = cp12 bench (rc1/rc2) unless stated.

| ID | What | State | Evidence | Device-verified |
|---|---|---|---|---|
| V1b/V2/V3 | Voice real-time per stream: v2 tag, RTP jitter buffer (target `audioLatencyMs`), stock device buffers, NAV without focus, duck follows `duckAudio`/`unduckAudio`. **Replaces the designs behind A6, A7, A10 and A11's triggers**; `keepAlive`, `sweepIdle`, `TRACK_BUFFER_MIN_MS` and the energy gate are deleted | LANDED | `av/VoiceJitterBuffer.kt`, `VoiceStreams.kt`, `VoiceLatency.kt`, `DuckRamp.kt`, `FocusSlot.kt` | Partly: nav live/no focus, platform duck, 0-loud-excess start (`04` §4e); B-V1-W matrix pending |
| fix2 | Start-stall exemption, `NAV_DUCK_LIFT_ON_BUS`, `VOICE_THREADS_URGENT_AUDIO` | LANDED | `VoiceJitterBuffer` start-stall block, `VoiceStreams.navDuckLift`, `VoiceRouter.raisePriority` | Lift and start observed on rc2; URGENT_AUDIO CPU effect not measured |
| V5 / Q001 | Per-install pairing key + OD-8B automatic Forget Pairing (row T6.5) | LANDED | `AccessoryKey.kt`, `RekeyFlow.kt` | Host JVM only; B-V5-1 pending |
| OD-7F | No new connection displaces an encrypted live session | LANDED | `CarPlayRxSlots.kt` (`Admission`, `REFUSE_SESSION_LIVE`) | Host JVM only |
| UI-1..3 | Recover -> Restart session; Box Info / Log Scope / Log Status removed; Adapter & logs sheet; Apply now | LANDED | `LauncherModel.kt`, `Ui.kt` | No: layout/keyboard pending |
| OD-7A | No compiled-in hotspot credentials; blank first run + dialog (owner: no rotation, no history scrub) | LANDED | `Ui.kt`, `HotspotPrefs.kt`, `SessionCore.kt` | No |
| D6 / OD-9D / OD-9E | Auto-claim with the app closed (attach receiver, boot, media-browser bind), Stop lasts one boot, one status notification | LANDED | `AutoClaim.kt`, `AutoClaimPolicy.kt`, `av/StatusNotice.kt`, `av/CarPlaySessionService.kt` | Media-browser-bind claim yes (rc2); other triggers, Stop-per-boot no |
| MED | Delta now-playing merge; idle-selectable media source; HIDDEN second launcher tile; OD-9C MainAudio take; focus reclaim on resume/screen return | LANDED | `av/NowPlayingMerge.kt`, `CarPlayMediaBrowserService.kt`, `AndroidManifest.xml` (`com.gm.homescreen.visibility` HIDDEN), `MainAudioHandover.kt`, `FocusReclaimRule.kt` | Icon + metadata kept across same-artist skips on rc1; take/reclaim pending (B-MED-9) |
| MIC | Bench-only `mic_inject` / `finish_launcher` behind `GMCCPA_BENCH=1` | LANDED | `bench/BenchHooks.kt`, `nobench/BenchHooks.kt` | n/a |
| D016 | Kotlin pairing fallback removed | LANDED | `CarPlayRx.kt` (no fallback); `pair/` holds only `NativeCore.kt` | n/a |
| Tier-0 | JVM tests in the gate (48 at 2026-11-12), flavor switch, dirty-tree stamp | LANDED | `tools/test.sh`, `tools/jvm_tests.txt`, `tools/build_apk.sh` | Gate runs host-side |

### 2026-10-08 remediation (Phases 1-7)

Basis: six review passes (A-F) plus a read-only bench-radio runtime review, every finding re-verified by three
independent agents; owner decisions D1-D9. Bench builds below: `230eaa7` (code 428, `install -r` over rc2) and
`6c95684` (code 430). Each row names its test where one is host-possible.

| ID | What | State | Evidence | Device-verified |
|---|---|---|---|---|
| E-F1 / owner rule | `tools/test.sh` built cargo output in-tree (~1.7 GB in the synced tree). Every cargo call now sets `CARGO_TARGET_DIR` under `$OCBM_CACHE/cargo-targets/`; first gate step `tools/tree_artifact_guard.sh` FAILS on any real `target`/`build`/`.gradle`/`__pycache__`/`.build`/`node_modules` dir under `host/gm_ccpa` or `crates/` (symlinks pass; `GMCCPA_ALLOW_TREE_ARTIFACTS=1` only while cleaning up) | LANDED | `tools/test.sh`, `tools/tree_artifact_guard.sh`, `06` §4 | n/a |
| E-F4 | `adb_radio_probe.sh` wrote beside itself (every later build `-dirty`); now `${GM_OUT:-/tmp}` | LANDED | `tools/adb_radio_probe.sh`, `.gitignore` | n/a |
| E-F5 | `build_apk.sh`: `git rev-list --count` failure is FATAL; `cargo build --locked`; kotlinc drift warning (`KOTLINC_EXPECTED`) | LANDED | `tools/build_apk.sh` | **Yes**, `230eaa7`: built and installed, versionCode 420 -> 428 |
| B-F1 | `SessionSupervisor.onBoxLost` (non-GRACE) cancels the ladder retry and resets `rung`/`skipped1` (a stale retry fired into IDLE under the relink); `onRelinkEnded` rewrites a STALLED whose cause is ADAPTER | LANDED | `SessionSupervisor` `onBoxLost`/`onRelinkEnded`/`stalledCause`; `SessionSupervisorTest.bf1_*` | Partially, `6c95684`: adapter unplugged under a live session ran grace + ladder (rungs 1-2 skipped `adapter: ABSENT`), accurate STALLED text, no `MGMT_RESTART_WIRELESS` after replug. Not the exact transport-death-mid-ladder case |
| C-F1 | Media focus REFUSED at start (MUSIC while a CALL holds focus): held silent via `pausedForFocus`, re-requested every `FOCUS_RETRY_MS` while media flows | LANDED | `AacPlayer.requestFocus` refusal branch, `FocusReclaimRule.shouldRequestFromFeed`; `FocusReclaimRuleTest.feedGate_*` | No, `6c95684`: the on-iPhone call arrived as CarPlay's telephony stream and focus was granted, so the refusal branch never ran. Needs a call on the head unit's own Bluetooth phone |
| A-F1 | Unauthenticated connect to `:7011` put the supervisor in SESSION_UP at accept. Now `sessionUp` + `onSessionUp` move to the pair-verified callback; the pump closes a still-plaintext connection 10 s after accept as `EndReason.UNVERIFIED` | LANDED | `UnverifiedConnRule.kt`, `CarPlayRx.handleNative`, `SessionCore.onPairVerified`; `UnverifiedConnRuleTest`, `SessionSupervisorTest.af1_*` | Partially: normal pairings reached pair-verify (28-41 ms) and were not cut by the deadline (4 pairings across both builds); first frame ~3.0-3.1 s after accept/pair-verify (re-sample vs the 2.598 s baseline in `12`). Pending: rogue non-local socket, first pairing after Forget (<10 s) |
| D-F1 | Notification and media-card `PendingIntent`s collided (code 0); distinct request code and action (`TapTarget`) | LANDED | `av/TapTarget.kt`; `TapTargetTest` | No: Stop, then tap the media card |
| D-F3 | `VehicleStateWatcher` Car callbacks were field initialisers (`NoClassDefFoundError` without `android.car`); now `by lazy` inside the try, construction `runCatching` | LANDED | `av/VehicleStateWatcher.kt`, `SessionCore.startCarPlayRx`; `VehicleStateWatcherConstructionTest` | n/a |
| B-F3 | Relink wait expiring with the adapter enumerated-but-unlinked held the LINK hold and notification forever; `onRelinkDone` uses `releaseLinkIfUnlinked` | LANDED | `SessionCore.onRelinkDone`; `AutoClaimPolicyTest.releaseRuleAtRelinkEnd` | Partially, `6c95684`: no-adapter release path seen; the ten-minute enumerated-but-ungranted case pending |
| B-F2 | A Stop during bring-up could not stop the `CT_SUBSCRIBE` radio-wake edge; `autoStart` passes `subscribe = { StartPolicy.subscribeAllowed(SessionHolder.stopRequested) }`; operator Stop uses `stop(quick = !hasLink())`. Not done: interrupting OcbmClient wait loops (76 s worst case); `runAllLocked` is do-not-touch | PARTIAL | `SessionCoreParts.StartPolicy`; `SessionCorePartsTest.startPolicy_*` | No |
| D-F2 | A launcher recreate re-delivered the `--es run` verb; verb runs only on a fresh delivery, retained intent stripped | LANDED | `MainActivity.dispatchIntent`, `SessionCoreParts.IntentReplay`; `SessionCorePartsTest.intentReplay_*` | No: rotate/recreate after a verb |
| D-F4 | Exported-verb gate | **CLOSED, owner decision D4: not coded.** Verbs stay open (the `06` §4/§8.2 shell recipes depend on it). Reopen on a non-bench user on the unit or a verb reachable from another app | — | — |
| E-F2 | `MGMT_INFO` logged in 110-char chunks let a split MAC escape the line-wise redactor; serial had no rule. Chunks break on JSON element boundaries (`MgmtInfo.logChunks`); Redactor gains SERIAL and BOX_NAME (`CarLink-xxxx`). Logcat itself stays unredacted by policy (`06` §8.2) | LANDED | `logging/Redactor.kt`, `ocbm/MgmtInfo.kt`; `RedactorTest`, `MgmtInfoTest.logChunks_*` | Partially, `230eaa7`: whole-OS export (12,260 lines): 0 full MACs, 0 literals of the hotspot SSID/passphrase; phone and home SSIDs leaked via WifiConfigManager `configKey "<name>"` lines (rule added same day, `RedactorTest.configKey*`, unit-tested only). Export held no `MGMT_INFO` line, so chunking not exercised |
| E-F3 | `RE_WPA` missed `wpa_passphrase=`, `password=`, `hotspot_pass=`; widened (bare `pass` still excluded). Redactor half of R6.6.29 | LANDED | `Redactor.kt` `RE_WPA`; `RedactorTest.passphraseSpellings` | Partially, `230eaa7`: 0 hotspot-passphrase literals in the export; new spellings unit-tested only |
| E-F9 | `start_epoch_ms=` lost to `RE_LONG_DIGITS`; exempted by lookbehind | LANDED | `Redactor.kt`; `RedactorTest.startEpochMsIsExemptFromLongDigits` | No: export held no `SESSION v=3` line; export after a session ends |
| E-F6 / E-F7 | Second export while the SAF picker is up refused ("export already pending"); `LogFiles.open()` after `close()` guarded | LANDED | `logging/LogExport.trySaf`, `logging/LogFiles.kt` | No (Android-bound) / n/a |
| E-DOC 12 | Printed `pm grant` recipe carried no `--user`; now `grantCmdForUser` | LANDED | `logging/LogCapture.kt` | Partially, `230eaa7`: `install -r` lost READ_LOGS (USB grant survived); the `06` §8.1 recipe restored whole-OS capture; whether the app's printed line was the one used is not recorded |
| A-F2 / A-X1 | All Rust stdout/stderr reached logcat at INFO. One pipe kept (cross-stream order; receiver prints success on stderr); JNI failure sites write `[jni] E: `/`[jni] W: ` markers mapped by `log_prio` | LANDED | `lib.rs` `jni_log`/`log_prio`; `log_prio_maps_markers_only` | Partially, `230eaa7`: no JNI failure occurred; 5 E lines from the app pid, all platform noise; 0 EXPECTED-MISSING/LATE; no crash/ANR |
| C-F7 | AUs the decoder never took (TRY_AGAIN, null buffer, oversize) uncounted; `ausNotQueued` in 500-frame and stop summaries | LANDED | `av/AacPlayer.kt` | **Yes**, `230eaa7`: first summary `1 AUs not queued, 3 underruns` (start transient), invisible before |
| A-X2 | `AccessoryKey.setRekey` fsync + `ATOMIC_MOVE` + dir fsync | LANDED | `AccessoryKey.kt`; `AccessoryKeyTest.rekeyFlag_publishedAtomically_noTempLeftBehind` | n/a |
| F-C8 | PHONE board "DOWN (cleared by box, no phone identified)" under live CarPlay is expected in this role (box never sees SETUP); text says so; `onPhoneIdent` no longer erases a known identity | LANDED | `ocbm/OcbmClient.kt`, `logging/SessionSummary.kt` | No |
| 4.5 | "SETUP negotiated ... 0x0@0": `set_resolution(2400, 960, 60)` on the AvSession (log-only) | LANDED | `lib.rs` `nativeInit` | No: line not checked |
| D-F6 | Status notification text per state (closed set 7 -> 11) | LANDED | `av/StatusNotice.kt`; `StatusNoticeTest` | No: text not reviewed on the unit |
| D-F7 | `MdnsResponder.start()`/`reannounce()` blocked ~2-3 s; announce runs on its own daemon thread | LANDED | `MdnsResponder.announceAsync`; `MdnsResponderStopTest` | No: start-to-claim delta not measured |
| D-F9 / D-F11 | `Ui.release()` dismisses dialogs, post guards test `isDestroyed`; FGS start in `CarPlayActivity.startSession` is `runCatching` (a refusal runs the session unheld) | LANDED | `Ui.kt`, `av/CarPlayActivity.kt` | No |
| F-C2 | GC churn: grow-only scratch buffers in `HevcRenderer`/`AacPlayer`; NAL scan is a pure `(buf, len)` function | LANDED | `av/NalScan.kt`, `HevcRenderer.kt`, `AacPlayer.kt`; `NalScanTest.staleTailBeyondLenIsNeverScanned` | Indicative only, `230eaa7` (~3 min, not a controlled A/B): PSS 42.7 MB (rc2 39.2), threads 72 (rc2 77), app CPU 26.6 % of a core (rc2 ~32 %) |
| F-C3 | Launcher metrics applied from an `OnPreDrawListener`, not inside the layout pass | LANDED | `Ui.kt` | No: launcher covered while a session is live (D-F5) |
| C-F2 | Lock-order inversion: `VoiceJitterBuffer.pull` ran `fateOf` under the jb monitor; `navSuppressed()` snapshotted once per pull outside the lock | LANDED | `av/PurposeEngine.kt` `fateOf`, `VoiceJitterBuffer.kt`/`VoiceStreams.kt` headers | n/a |
| A-F4 / C-F5 | Dead code: `startCarPlayRx` (was `toggleCarPlayRx`), `Node.readUs` removed, `Node.removed` enforced | LANDED | `SessionCore.kt`, `av/VoiceJitterBuffer.kt` | n/a |
| Phase 10 | Pure moves (`PurposeEngine`, `ocbm/OcbmNames.kt`), `ModesChanged.decode` extracted; 5.12 test (see row 5.12) | LANDED | `av/ModesChanged.kt`; `ModesChangedTest`, `InfoTxtEqualityTest` | n/a |
| Comments / docs | A-F3, B-F4, C-F3/C-F4/C-F6, D-DOC 1-5, E-DOC 1-12 | LANDED | the cited files | n/a |

**Bench pass 2026-10-08, facts not tied to one row** (build `6c95684`; recovery procedures in `06` §4):
- A plain `install -r` keeps the persistent USB grant but not READ_LOGS (`06` §8.1). iPhone log: 0 Identify rejects.
- Two startup warnings are observations, not regressions: `ForceKeyFrame refused by the event channel — no IDR is
  coming` after pair-verify (row BP-4) and `BT phase went BACKWARDS WIFI_HANDOFF -> IDLE while the box reports
  PROJ_MODE NONE`.
- `export_log` launches the SAF picker, which the CarPlay screen covers (D-F5); the export stays pending until
  MainActivity resumes, then falls back to MediaStore Downloads (`06` §8.2).
- Adapter unplug under a live session: CarPlay kept running; the phone-side break stopped focus and the voice router
  cleanly (B-F1). Phone Wi-Fi/BT down: connect-outs to the cached endpoint timed out (10 attempts) and NsdManager
  resolves failed `code=3` (ALREADY_ACTIVE) every ~12 s until Stop (BP-1).
- Box reconnect after Stop/Start: `[reconnect] 1 bonded phone(s)` paged the phone 5 s after btd's HCI bring-up (live,
  first frame ~3.0 s after pair-verify). In earlier bring-ups no reconnect line appeared: cause unknown, and
  unorderable from the box log because `[old]` backfill lines carry the relay time (BP-2). `bt_on` set
  `ccpa-<4hex>` then `btd` set `CarLink-<4hex>` (`docs/wireless/01_BT_AND_RADIO.md` §6c); the phone sees only the
  second.
- iPhone log: `Bluetooth power state: ffffffff` is the normal reply (a toggle shows `0` / `cafebabe`); "Failed to
  parse accessory cert chain" belongs to an audio accessory (box MFi auth: AuthSuccess).
- `am force-stop` does not keep the app closed; the in-app Stop does until next boot; verb stop + launcher reopen
  reached CarPlay live in ~4.9 s (`06` §4).
- adb transport dropped ~13 min with no reboot (uptime 2:19, boot reason `cold`, same app pid); cause unknown.

#### Still pending bench checks

Open on the bench radio:
- Voice: B-V1-W matrix (10 nav + 3 Siri overlaps + 5 Siri), B-V3-3, B-MED-9 (does iOS pause on the MainAudio
  take), media net-underrun rate against cp12's ~1/min, FM -> CarPlay resume.
- Pairing: B-V5-1 (iOS response to the key change), first pairing after Forget (<10 s deadline, A-F1), rogue
  non-local socket on the vehicle hotspot (A-F1).
- Lifecycle: auto-claim triggers other than the media-browser bind, Stop-per-boot, launcher Stop then media-card
  tap (D-F1), the 10-minute enumerated-but-ungranted relink (B-F3), N13/N17 acceptance.
- Audio focus: a call held on the head unit's own Bluetooth phone (C-F1).
- UI/logs: launcher layout/keyboard and gfxinfo/screenshot (F-C3; the launcher is covered while a session is
  live), an export taken after a session ends (E-F9).
- Steering-wheel Voice button -> Siri (`06` §4).

#### Opened by the 2026-10-08 evening bench pass (not coded)

| ID | What | State | Evidence | Device-verified |
|---|---|---|---|---|
| BP-1 | While the phone is away the app dials a STALE cached `_carplay-ctrl` endpoint and NsdManager resolves fail `code=3` every ~12 s until Stop; first fresh resolve only after the phone rejoined | **OPEN** | `CarPlayRx.rediscoverLoop`/`connectOut`; logcat 19:54:18-19:56:39 | Yes, observed |
| BP-2 | `CH_LOG` backfilled `[old]` lines are stamped with the relay time, not the box event time. Fix: carry the box timestamp (box side is static code: record; app side prints it if a frame ever carries one) | **OPEN** | the 19:56 bring-up | Yes, observed |
| BP-3 | A phone call routed as CarPlay's telephony stream is logged "assistant SPEAKING ... media paused for the assistant" (`phone=1`); check whether the label should distinguish a call from Siri | **OPEN** | `6c95684` call test | Yes, observed |
| BP-4 | `ForceKeyFrame refused ... no IDR is coming` right after the renderer attaches at every session start; confirm it cannot leave the screen black (it did not) | **OPEN** | `6c95684`, every session start | Observed; effect unconfirmed |

#### Closed, owner-tracked (not coded; each with its reopen trigger)

| ID | What | Why not coded | Reopen when |
|---|---|---|---|
| F-C1 | Box `wireless` daemon's 1 Hz `EADDRINUSE` retry loop (SDP/HSP/HFP) = 32-37 % of the app's log lines | Static box code; the holder of PSM 1 / RFCOMM 5-6 was never identified | Owner opens a box console (CH_CONSOLE `ss`/`fuser`); then upstream log-once-per-streak |
| 8.2 | Pair-setup fixed code `3939` (`lib.rs` F141): any hotspot client can reach ENCRYPTED and lock the phone out via OD-7F | Design/upstream: the code is what iOS expects; per-install needs a receiver-crate change | A hostile client is observed on the hotspot, or the receiver gains a setup-code setter |
| 8.3 | No source timestamps over the seam; `/feedback` answered with an empty 200, so the phone gets no rfps/ltnc stats (levers for the ~30 fps cadence) | Static receiver code | Owner reopens Phase 9 (FPS) with iPhone evidence (`idevicesyslog -p AirPlaySender`) |
| A-F6 / T5.1 | `std::env::set_var` in `JNI_OnLoad` after threads may exist | Fix is the T5.1 refactor | T5.1 |
| B-F5 | `host/CarlinkAndroid`'s `Mfi.parse` lacks this app's bounds guard | Other project; latent | Next CarlinkAndroid session |
| DO LATER | `handleCtrl` arm split, `PumpIdleRule`, `DialPlan`, `DnsWire` golden vectors, `HeartbeatPolicy`, `OcbmProbe` transport injection | Need the adapter on the bench and the bench checks passed first; one commit each, test-first | After the pending bench checks |
| 6.3 / F-C4 | Render pacing (`releaseOutputBuffer(idx, renderTimeNs)`): the 1-/3-vsync bimodal cadence | Owner decision D6, not adopted (~1 frame latency; touch risk); cause in `12` Phase 7. Re-measured on `230eaa7`: 33 fps decode, SurfaceFlinger avg 34.9 ms, 1/2/3/4+-vsync = 53/13/55/5, same as rc2. Owner accepted ~30 fps (iOS adapts encode rate to content) | Owner opts in; measure with `SurfaceFlinger --latency` |
| F-C5 | Platform `ethctrlmgr` ~1.2k lines/s floods the 1 MiB main ring; the app's logcat child ~3.4 % CPU | Platform; `persist.logd.size` is a device-owner action (`06` §8.4) | Owner may raise the ring size |
| D9 | Upstream proposal: app pushes a `bt_name` so the adapter is not `CarLink-<Wi-Fi-MAC suffix>`; needs `ocbmd` (`bt_name_from`), `btd`/`bt_bringup`, `bt_on.sh` | Static box code. Costs: a rename invalidates the bonded record (forced re-pair); two adapters on one SSID-derived name collide; the SSID is broadcast over BT | Owner asks; then a `SubscribeConfig` key plus the three box-side readers |

### Audio (A6-A11), from the 2026-09-09 capture

Measured on hardware; firmware facts from the unit's `/vendor/etc/audio_policy_configuration.xml`. Owner doc:
[`13_AUDIO_ROUTING.md`](13_AUDIO_ROUTING.md). Rows A6, A7, A10 and the trigger half of A11 are closed by V1b/V3; each is kept as one line with the measurement that motivated it.

| ID | What | State | Evidence | Device-verified |
|---|---|---|---|---|
| A6 | `VoiceRouter` dropped 17 % of Siri AUs (`keepAlive` double-feed filled the 400-640 ms buffer) | CLOSED by V1b | 249 AUs x 30 ms = 7.47 s vs sink life 7.22 s; all 42 drops were the full-buffer branch | Defect confirmed on hardware |
| A7 | Energy-gated duck pumped on a paused track; `sweepIdle` released sinks before un-ducking | CLOSED by V1b/V3 | `setVolume` edges landed on a track paused 22:55:39-52 | Defect confirmed |
| A8 | `AacPlayer` media buffer was rate-blind and smaller than one delivery pause (85-160 ms vs ~200 ms); `feed` also counted discarded-while-paused output as played (~240 frames over-count) | **IMPLEMENTED** (in the committed tree) | `TRACK_BUFFER_MS = 750L` with a `maxOf(minBuf * 2, ...)` floor; `feed` classifies PLAYED / DISCARDED_PAUSED / NO_TRACK / NO_PCM. Target `0 underruns` after the counter fix, first-audio latency reported as the cost | No: 6 underruns in 843 frames is the pre-fix baseline |
| A9 | Build voice sinks at 48 kHz stereo | **CLOSED, refuted** | An `AudioTrack`'s declared format is the format of the bytes written; the only rate knob is AudioFlinger's resampler, which a 16 k track already uses; `getMinBufferSize` already scales by the output-thread rate ratio (`AudioTrack::getMinFrameCount`). Cost of status quo: <1 % of an Atom core. Real win was `TRACK_BUFFER_MIN_MS` (A10) | n/a |
| A10 | Siri's first word delayed by the 400 ms track pre-fill | CLOSED by V1b (pre-fill floor removed with the keep-alive design) | instrumentation `[voice] siri: FIRST AUDIO OUT +Nms after configure` | Not measured |
| A11 | Voice state inferred from int16 peaks and quiet timers while iOS sends explicit `modesChanged` on the control channel (`appStates[]{appStateID, entity, speechMode}`; `speechMode -1 -> {0..3}` is exactly -10 B; `appStateID 1` is Speech) | Trigger half closed by V1b/V3; decode landed in Kotlin only | `CarPlayActivity.onModesChanged`, `av/ModesChanged.kt`; `docs/ops/captures/2026-07-24_carplay_cmd_capture.bin` (35 frames). 6-for-6 / 14-for-14 bracket in the 2026-09-09 capture: iOS signalled Siri 317 ms before our gate opened and its end 3.27 s before our inference. `emit_command_plist` `try_lock`s and may drop the plist, so an energy fallback is mandatory. `duckAudio` now drives the duck (V3); `playbackStatus` is deliberately not wired into `AacPlayer` (stale "playing" would pin a dead track active) | Parse verified by running `BPlist.parse` over the stored capture |

Verification rule: a green build is evidence of nothing; each audio row needs a named log line or measured
number off the truck before leaving OPEN.

### Found by the 2026-09-09 capture (N17-N25)

| ID | What | State | Evidence | Device-verified |
|---|---|---|---|---|
| N17 | `ControlServer::av_idle_ms()` not exported over JNI, so `handleNative` broke the control connection on its 30 s read timeout while A/V flowed | **IMPLEMENTED 2026-10-06 (v5.5)** | `Java_zeno_gmccpa_pair_NativeCore_nativeAvIdleMs` <-> `NativeCore.nativeAvIdleMs(Long)`: `try_lock`; -1 no A/V ever, -2 unknown (contended, generation mismatch, panic, stale `.so`). The pump `continue`s only while 0 <= av < 30 000 ms, else `LINK_LOST` (reference: `WouldBlock` arm of `serve_connection`, `net.rs`). The export lives in this app's `carplay-jni`; the crate is untouched | No: pending acceptance |
| N18 | An `ocbmd` restart between the `CH_MFI` cert and sign leaves the replacement daemon with no host nonce (it serves later ops without a HELLO) | **IMPLEMENTED 2026-09-10** | `OcbmProbe.runAllLocked` with `REHELLO_TIMEOUT_MS = 4_000L`: after a sign timeout that follows a good cert, re-HELLO once and retry the sign once. Cost: +15 s worst case on a real chip fault, +4 s on a wedged daemon. The box-side "wedged" verdict was a false positive against a healthy idle daemon (box-side, out of scope) | Defect observed on hardware (cert 160 ms, sign timeout 15 s); fix not exercised |
| N19 | MFi timeouts were not recorded in `SessionSummary` | **IMPLEMENTED 2026-09-10** | `OcbmProbe.runAllLocked` `cert == null`/`sig == null` branches and all four `mfiRelay()` methods now call `onMfiFailure` (relay failures prefixed `relay `; a sign timeout recovered by N18 records `sign: timeout, recovered by re-HELLO`) | Defect confirmed |
| N20 | No `SESSION` line for a launch that is not a USB attach | **IMPLEMENTED 2026-09-10** (schema v=2 at the time; now v=3, `SessionSummary.SCHEMA_VERSION`) | `Origin` enum (`usb_attach`/`launch`), `origin=` appended per the append-only contract, `SessionSummary.beginLaunch(dev)` never fabricates (`perm_trampoline=none`, `serial=unknown`); `permissionDialogObserved()` with a 15 s pending-stamp window; `exit=launch_superseded_by_attach` | Defect confirmed |
| N21 | `UsbAttachActivity` banner asserted the reverted fixed-handler squat | **IMPLEMENTED 2026-09-10** | now `USB ATTACH TRAMPOLINE 0x%04x:0x%04x — permission held=%b, ccpa=%b`; `OcbmProbe.awaitClaimable` comment fixed; nothing greps the old string | n/a |
| N22 | Two `AacPlayer` races: (a) `requestFocus` wrote `pausedForFocus = false` outside the monitor and `reclaimFocus` did a check-then-act; (b) `applyPauseState`/`feed` reclaim not gated on `running` | **IMPLEMENTED 2026-09-10** | all GRANTED bookkeeping in one `synchronized(this)`, refused when `!running`; `stop()` writes `running=false` under the same monitor; request/callback sequence stamp; `abandonFocus` owns "no focus means no focus duck". The consume thread does not park in a blocking write during a hold (`AudioTrack::obtainBuffer` is non-blocking when not ACTIVE) | No |
| N23 | Probe scripts enumerated user 0 only (this app is user 10) | **IMPLEMENTED 2026-09-10** | `USERS="${USERS:-0 10}"` and `sh_users` in `tools/adb_radio_probe.sh` and `tools/deepprobe.sh`. The 2026-09-09 bundle was not produced by either script (trap note in `01` §3): regenerate it with the fixed scripts | n/a |
| N24 | The app narrated success and stayed silent on failure (reference session: 634 `I` / 7 `W` / 0 `E` around a 15 s MFi timeout, 42 dropped AUs, 6 underruns) | **IMPLEMENTED 2026-09-10** (`6c6ec79`) | `logging/SessionTrace.kt`: `!! EXPECTED-MISSING` (E) / `~~ EXPECTED-LATE` (W) / `== EXPECTED-MET` (I), `## STATUS` board; `IDENTITY`/`PLATFORM` anchors and `assets/build_sha` make the PID filter constructible. Recipe `06` §8.3 | Two true positives expected on first run (`av/voice-first-out/alert`, READ_LOGS marker) |
| N25 | READ_LOGS "assigned at fork" explanation may be wrong: a process forked at boot reported `read_logs=true` yet degraded to own-process with no `pm grant` that boot | Self-heal IMPLEMENTED 2026-09-10; **root cause OPEN** | `ScopeMarker` persists the grant observation, degraded pid and process start time so the next start re-requests WHOLE_OS and a fresh-process failure prints a different remediation; `grantCmdForUser` derives `--user` | Detection confirmed; cause not |

### Discovery / mDNS (R6.1-R6.2, 2026-08-27)

| ID | What | State | Evidence | Device-verified |
|---|---|---|---|---|
| N12 | `capturing requested=... effective=... read_logs=...` prints resolved values on the pump thread | LANDED | `logging/LogCapture.kt` | **Yes** |
| A1 | Two-arg `joinGroup` at all three 5353 sites | LANDED | `MdnsResponder.start()`, `MdnsInspect.inspect()`, `MdnsInspect.liveEndpoint()` | **Yes**: br0 multicast users 1 -> 2, `ENODEV` gone |
| — | mdnsd leg: A1 alone sufficed; speculative NsdManager re-registration not built | RESOLVED | doc record | **Yes** |
| RFC 6762 | AAAA answered with A + NSEC; announce spacing 250 ms -> 1 s | LANDED | `MdnsResponder.handleQuery()`, `recNsec()`, `announce()` | Partially: AAAA/NSEC path not isolated |
| — | Capture always-on from `onCreate` | LANDED | `LogCapture.start` | **Yes** |

### Box side (`ocbm`)

| ID | What | State | Evidence | Device-verified |
|---|---|---|---|---|
| Radio seam | `radio_hal.sh`/`radio_detect.sh` missing from the deployed supervisor: no BT bring-up, no error | **LANDED 2026-08-28** | `docs/ops/06_CORRECTIONS_LEDGER.md` R-20W-5; `check_supervisor_deps()` in `tools/ocbm_push.sh` | **Yes**: `hci0 UP RUNNING` from cold boot |
| N3 | `pgrep -x`/`pkill -x ocbmd` (was `-f`, matched the respawn wrapper) | LANDED | `restart_ocbmd_daemon()` in `tools/session_supervisor.sh` | Unverified |
| N4 | Reboot budget write-verified after `sync`, fails closed | LANDED | `FAIL CLOSED (audit N4)` block in `escalate()`, `session_supervisor.sh` | Unverified |
| 3.3 | `/tmp/bt_phase` un-latched on exit/teardown/start | LANDED | `rm -f /tmp/bt_phase` in `wireless_down()` | Unverified |
| 5.7 | BT SYN resend capped | LANDED | `SYN_RESEND_MAX = 9` in `crates/vendor/wireless/src/bt_driver.rs` | Unverified |
| 3.4/3.5 | Split HOST_GONE emission; `F_REPLAY` bit2 semantics | 3.5 **LANDED**; 3.4 **UNVERIFIED** (half) | `F_REPLAY` (bit2, `crates/ocbm-proto/src/lib.rs`) is set in `mirror_flags` (`box/ocbmd/src/main.rs`) on every state-mirror replay and consumed in `OcbmClient.dispatch`. `SEV_HOST_GONE` is emitted from several ocbmd sites (unsubscribed-heartbeat answer, host-gone teardown); whether the split matches the audit's intent was not re-derived | No |
| R6.5 | TTL-gated deploy trial / dead-man before flashing a daemon | Standing procedure, not a row | — | — |
| R6.6.27-28 | Rotate AP passphrase; rotate `edSeed`/setup-code fallback | **OPEN** | The `srp.rs` `b"3939"` hits are all inside `#[cfg(test)]`, not a runtime credential. The box-side setup code is the literal `b"3939"` in `run_pairing_server` (`box/carplayd/src/main.rs`); the edSeed fallback is `ED_SEED_FALLBACK` in the same file. `edSeed` rotation: T6.5 | No |
| R6.6.29 | Scrub tracked credential files; widen `RE_WPA` | scrub **OPEN**; widen **LANDED 2026-10-08** | `boards/rpi4-aaos/evidence/hostapd_5g.conf` still tracked, no box-side `RE_WPA`. The app-side `RE_WPA` (`logging/Redactor.kt`, E-F3) is export redaction, not coverage for the box-side scrub | No |

R6.6.27-29 are real, unremediated exposures in the box side. This doc does not fix them. Do not print the actual
passphrase or seed into any doc; use a placeholder and rotate before any further sharing of that repo.

---

## Upstream `ocbm` drift: standing procedure

`gm_ccpa` links `receiver`/`pairing`/`mfi` as in-tree path deps (working tree, not a pinned rev; row T0.1).
Last manual review: 2026-08-12 (pre-import standalone-repo hash, no longer resolvable). Before trusting a later build:

1. Note `git rev-parse --short HEAD` of the checkout and diff the crates against the last reviewed state.
2. Re-scan `receiver`/`pairing`/`mfi`/`ocbm-proto` for changes to `forward.rs`/`stream.rs`/`server.rs`/`net.rs`/
   `datastream.rs`/`iap_tunnel.rs` (the `:9001`/`:9002` seam contract) and `levers.rs`/`session.rs` (the
   `OCBM_FWD_ENC` and lever-write contract).
3. Re-verify on every advance: `OCBM_FWD_ENC=0` is set in `JNI_OnLoad` (`set_var("OCBM_FWD_ENC", "0")`, `lib.rs`) and
   `levers::fwd_enc()` treats `0`/`false`/`off`/empty as opt-out. gm_ccpa's one-shot `levers::set_hevc(true)` holds only
   because it never calls `VehicleConfig::apply()`; if that changes, `hevc` can drop out of the SETUP
   `enabledFeatures` echo while the static `/info` still advertises `hevcInfo` (a session that dies ~21 ms after RECORD).

---

## Reference: verification tiers and regression risk

Tier 0: host `cargo test -p receiver -p pairing` plus the JVM classes, in the build gate. Tier 1: host replay binary
driving `serve_connection`/`ControlServer::handle` over fixture captures. Tier 2: emulator lifecycle exerciser
(hijack, 500x connect/kill/restart UAF hunt with CheckJNI, service-survives-Activity-finish). **T-REG** is the
standard truck regression: revive logging, force-stop, full run, phase-2 SETUPs, first-frame/first-audio
assertions, touch, 60 s soak, socket check, teardown with 0 AUs dropped. The `tools/truck_reg.sh` script is not in
this tree; the manual full-A/V procedure is `06` §7.1. Run it on a known-good build first to prove the rig.

Regression risk if T2.2/T5.1/M7 are picked up: the service-ownership move is highest risk (re-homes every working
object; slice it behind a flag); the handle registry's risk was a silent stall, not a crash (mitigated by the
INC-1/INC-2 split, landed); M7 discards the only device-proven accept loop (keep the Kotlin listener
runtime-selectable); the HEVC split fails as invisible stutter (never drop a keyframe/param-set AU); T5.1 reintroduces
the `-16720` desync class (dual-path assert, never delete the old path in the same commit).

---

## R6.10: box BT regression, 2026-08-27 (resolved 2026-08-28)

A `session_supervisor.sh` deploy without `radio_hal.sh`/`radio_detect.sh` left the box at `BOX_HEALTH 0x50`
(`HCI_PRESENT` clear, no Bluetooth) while OCBM claim/HELLO/MFi/SUBSCRIBE all succeeded. Read `CT_BOX_HEALTH` bit 0
first (`00_HANDOFF.md` §7). `tools/ocbm_push.sh` warns on a supervisor push missing the radio seam
(`check_supervisor_deps()`). Standing rule: never overwrite a box file without saving the original first.
Incident account: `docs/ops/06_CORRECTIONS_LEDGER.md` R-20W-5.

---

# Appendix A: the 2026-08-09 review (compressed)

The ledger outranks this appendix; it is kept for reasoning that still guides open work. Every finding from the
original review maps to a ledger task ID and was re-adjudicated there; per-severity counts are not retained. Do not
act on a severity from here without checking the ledger.

**Headline.** The architecture was sound. Defects clustered into five themes, closed by one structural fix (a session
object with a generation counter shared across the Kotlin/Rust boundary) plus config unification, build
reproducibility and a failure-surfacing pass. Wire protocol, SRP-6a and OCBM framing were verified correct; the gaps
were lifecycle, failure surfacing and build reproducibility.

**Language/architecture verdict (settled).** Keep the Kotlin shell + JNI'd Rust core (keep-and-refine 4.08 vs all-Rust
3.28 vs all-Kotlin 3.18, three independent lenses). All-Kotlin is dominated by the cost of re-proving ~19k loc of
iPhone-validated protocol behaviour on a truck where logcat is the only debug channel; all-Rust fights Java-only
framework APIs (`MediaCodec`/`AudioTrack`/`NsdManager`/`UsbManager`; NsdManager was empirically the only thing that
entered iOS's endpoint index).

**Why Rust must eventually own the control listener (M7, deferred).** `receiver/src/session.rs` binds every data-plane
socket itself; only the control connection is external, via `receiver::net::serve_connection<T: Read+Write>`. A
Rust accept loop recovers connection hijack and `TCP_KEEPIDLE/INTVL/CNT` dead-link detection and lets
`nativeFeed`/`nativeIsEncrypted` disappear. `run_pairing_server` (`box/carplayd/src/main.rs`) is the reference; the
Kotlin accept loop would be re-authored from it, not ported, with a new upward `SessionListener` callback.

**Why the `iap_tunnel` MFi routing needed a signature change (T5.3, landed).** `ControlServer` owned its signer by
value and `iap_tunnel` ran on a spawned thread, so routing its two chip call sites through one signer required
threading an `Arc<Mutex<dyn MfiSigner + Send>>`. C4 (the CH_MFI correlation gap) had to land first: routing the second
consumer through the client made the correlation bug live rather than latent.

**Platform work carried forward.**
1. The box's own A/V layer is not suppressed in the bridge role: the supervisor lets the box's `carplayd`/`airplayd`
   start (harmless with `wifi_ap:false`: no route to the vehicle subnet; they latch `AV_LAYER_UP` instantly).
   Suppressing them properly needs an explicit `AV_DISABLED` gate in `av.rs`, a Rust change
   (`tools/session_supervisor.sh`, `AV_SUPPRESS=''` comment).
2. Box identity derives from the Wi-Fi interface MAC first, then SoC OTP serial, then `/proc/cpuinfo` `Serial`, then
   the fixed `box-fallback` id (`crates/vendor/wireless/src/box_identity.rs`). A box that reads none of them falls back
   to one shared identity: harmless with one adapter, wrong with two.
3. Restore test inhibits before shipping: `/tmp/no_escalate`, patched `session_supervisor.sh`, `/etc/hostapd.conf`
   (from `.stock`).

**Risks that outlived the plan**

| Risk | Mitigation |
|---|---|
| Identity drift across four representations (TXT `features`, `/info` `features`, `deviceid`/`pi`, decimal MAC in the connect-out header) | A golden-identity test deriving all four from one source and byte-comparing |
| Silent-failure protocol traps: nine failure modes produce no diagnostic | Log every unhandled route loudly; never answer with a fake 200 |
| Negotiating unimplemented features kills the session ~21 ms after RECORD | Implement, then advertise, never the reverse |
| iPhone log redaction (`<private>` without Apple's CarPlay logging profile) | Preflight assertion: fail loudly if a known reason string comes back redacted |
| Bluetooth is the single point of failure: no cable fallback | Keep BT health visible |
| `/tmp/no_escalate` is tmpfs; one flap re-arms the reboot ladder | Re-assert every session start; power-cycle the adapter, not the truck |
