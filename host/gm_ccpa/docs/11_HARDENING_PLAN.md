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
| C2/T1.4–1.7 | Connection hijack, per-generation destroy, rediscover-on-session-end | LANDED | `netprobe_app/.../CarPlayRx.kt` (`currentSocket`, `acceptLoop`) | No |
| — | `OcbmProbe.runAll()` returns `LinkResult`, not `Unit` — launcher no longer claims "MFi proven" with no adapter | LANDED | `OcbmProbe.LinkResult` (`ocbm/OcbmProbe.kt`) | No |
| — | `SessionHolder` — receiver/probe/sink made process-scoped | LANDED | `MainActivity.kt`, `CarPlayRx.kt` (search `SessionHolder`) | No |
| — | `NativeCore.BUSY` bounds `nativeInit` on CORE contention | LANDED | `NativeCore.BUSY` (`pair/NativeCore.kt`) | No |
| C3/T2.1 | Foreground service + manifest `<service>` | LANDED | `av/CarPlaySessionService.kt` | No |
| T2.2 | Full session-ownership relocation into the service (survive Activity *destruction*, not just backgrounding) — scoped to `cpRx`/`ocbmProbe` only; does NOT cover re-seeding the per-Activity observers (`MainActivity.sessionUp`, `VehicleStateWatcher`, the supervisor's phase) | **OPEN** | not found; service exists but the risky slice (moving `cpRx`/`ocbmProbe` ownership) is not confirmed done | No |
| T4.8 | Serialize MainActivity command dispatcher through one executor; guard destructive verbs | LANDED | `MainActivity.kt` `cmdExecutor` (single-thread) | No |
| — | `CarPlayActivity.onSessionEnded()` tears the screen down on session end | LANDED | companion `onSessionEnded` in `av/CarPlayActivity.kt` | Partial — device-observed cold-start bug this fixed; teardown path itself not separately re-driven |
| — | `AacPlayer.reclaimFocus()` recovers media after permanent `AUDIOFOCUS_LOSS` | LANDED | `av/AacPlayer.kt` `reclaimFocus()` | No |
| — | `AacPlayer` full focus-listener + duck/pause/restore (R6.7 item 31) | LANDED | `av/AacPlayer.kt`: the `AudioFocusRequest.Builder(...)...setOnAudioFocusChangeListener(focusListener)` call in `requestFocus()`, and the `focusListener` lambda (ends just above `focusName()`) | No |
| C4/M3/T3.1–3.4 | `OcbmClient` `mfiLock`, length-correlated MFi responses, `Mfi.parse` truncation reject, `Tlv8` consecutive-only coalescing | LANDED | `ocbm/OcbmClient.kt`, `ocbm/OcbmProto.kt` | No |
| T5.3 | Gate `mfi-i2c-local` behind `local-mfi` (off on Android); route `iap_tunnel` through `Arc<Mutex<dyn MfiSigner>>` | LANDED | `receiver`'s `local-mfi` feature in `ccpa_custom crates/vendor/receiver/Cargo.toml`; the `receiver::iap_tunnel::set_remote_signer(...)` call in `Java_zeno_gmccpa_pair_NativeCore_nativeInit` (`native/carplay-jni/src/lib.rs`) | **Yes — 2026-09-09.** Tunnel got real `0xAA01`/`0xAA03` round trips through the OCBM relay (945 B cert, 128 B sig), reached `Identified`, and subscribed all three metadata channels. Zero `lock busy` lines. See `12_OBSERVED_FLOW.md` |
| — | Box-log streaming into logcat — **now `CH_LOG` push, not `CH_FILE` poll** (relabelled 2026-09-10: the row's title lagged the mechanism). Live streaming is `OcbmClient.handleLog` armed by `OcbmProbe.startBoxLogStream()`; `CH_FILE` survives only as the end-of-session snapshot | LANDED | `OcbmProbe.captureBoxLogsNow()` (`ocbm/OcbmProbe.kt`), `OcbmClient.filePull()` (`ocbm/OcbmClient.kt`); stale line cite, re-anchored 2026-09-10 — the old ranges are now `OcbmProbe.stop()` and `OcbmClient.startHeartbeat()` | **Yes** |
| T5.1 | `JNI_OnLoad` env-var config → one explicit `nativeInit` config; `/info` generated from same config | **OPEN** | the `std::env::set_var(...)` block in `JNI_OnLoad` (`native/carplay-jni/src/lib.rs`) still present | No |
| M7/T5.4 | Rust owns the control `TcpListener`/accept loop | **DEFERRED** (by design — hardware-gated) | no `TcpListener` in `lib.rs` | No |
| T6.4 | Gate `CARPLAY_SCREEN_DUMP`/`CARPLAY_SETUP_DUMP`/`.verbose(true)` behind a debug flag | **CLOSED 2026-09-10 — the premise was wrong, do not gate these** | The unconditional `std::env::set_var("CARPLAY_SCREEN_DUMP", "1")` in `JNI_OnLoad` (`native/carplay-jni/src/lib.rs`). **RETRACTED: it is NOT per-frame.** `spawn_screen` in `crates/vendor/receiver/src/session.rs` guards the dump with `if screen_dump && frames < 6` — at most **six lines per session**, and the 2026-09-09 capture contains exactly six (`DUMP frame#0` … `#5`). The earlier claim in this row, that it writes a hex line per video frame and is per-frame log volume on the A/V path, was a wrong generalisation from seeing frames 0–5 in a grep; commit `89c5c97`'s message repeats it and is likewise wrong. Those six lines are the only in-band codec identity a capture carries (they are what proved `hvc1` and the sample-description wrapping), so gating them off by default would remove evidence for no volume saving. `.verbose(true)` on `ControlServer` gates control-plane lines only — one per request plus pair-setup/verify and the `auth-setup (MFi-SAP) OK/FAILED` verdict, which is the receiver's own word on the `CH_MFI` relay — and is likewise kept. `CARPLAY_SETUP_DUMP` remains deliberately unset for the reason its own comment gives (unbounded per-SETUP files in an unreadable private dir) | Bound verified on hardware: 6 lines in a 1358-frame session |
| T6.5 | `SecureRandom` `edSeed`, persisted beside peer store (invalidates pairings — ships alone) | **OPEN** | the `edSeed` constructor default in `CarPlayRx` (`CarPlayRx.kt`) still `ByteArray(32) { (it*31+7).toByte() }` | No |
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
| N13 | TCP keepalive not armed **on the Kotlin path**: `java.net.Socket` cannot express keepalive idle/interval/count; `android.system.Os.setsockoptInt` can. The reference arms 3 s idle / 3 s interval / 3 probes (~12 s dead-link detect) | **OPEN** — found 2026-09-09, previously untracked; scope corrected 2026-09-09; evidence re-cited 2026-09-10 | Reference **does** implement it: `fn arm_keepalive` (`ccpa/carplayd/src/main.rs`), called per control connection in `run_pairing_server`. Gap is this app only: the `NOT YET IMPLEMENTED` comment block in `CarPlayRx.kt` (search `arm_keepalive`), which correctly names this as a Kotlin-side gap; its formerly stale cross-references are N16 | No |
| N14 | `start_input_listener()` HID endpoint on `127.0.0.1:9110` not implemented **on the Kotlin path** — this app calls `send_hid_report` in-process instead of over the reference socket | **OPEN** — found 2026-09-09, previously untracked; scope corrected 2026-09-09 | Reference **does** implement it: `fn start_input_listener` (`ccpa/carplayd/src/main.rs`, binds `127.0.0.1:9110`; started from `main()` before the accept loop). Gap is this app only: the doc comment above `Java_zeno_gmccpa_pair_NativeCore_nativeTouch` in `native/carplay-jni/src/lib.rs` (which now cites `fn handle_input_frame`, `ccpa/carplayd/src/main.rs`; its earlier `airplayd main.rs:938` citation was stale) | No |
| N15 | Per-connection `build_info(&load_device_config())` not implemented **on the Kotlin path** — a static `/info` asset ships instead of a per-connection rebuild | **OPEN** — found 2026-09-09, previously untracked; scope corrected 2026-09-09 | Reference **does** implement it: `pub fn build_info` (`crates/vendor/receiver/src/info.rs`), `fn load_device_config` (`ccpa/carplayd/src/main.rs`) called per control connection in `run_pairing_server`. Gap is this app only: the `build_info(&load_device_config()) per connection` comment block in `JNI_OnLoad` (`native/carplay-jni/src/lib.rs`), which calls `receiver::info::build_info(&cfg)` once for its side effect | No |
| N16 | Three stale cross-references in one source comment — the `NOT YET IMPLEMENTED` block in `CarPlayRx.kt` (search `arm_keepalive`): (1) pointed at `01_FINDINGS.md` §8 for these gaps, but §8 is now "What to protect from the debloat" — correct tracking is N13–N15 here; (2) cited `main.rs:407-432` (no binary named) for `arm_keepalive` — actually `fn arm_keepalive` in `ccpa/carplayd/src/main.rs`; (3) cited `server.rs:119` for `av_idle_ms` — actually `ControlServer::av_idle_ms` in `crates/vendor/receiver/src/server.rs` | **LANDED** in `8cadba4` (2026-09-10) — found 2026-09-09 | Verified 2026-09-10: `CarPlayRx.kt` at `HEAD` carries none of the three stale strings, and all three replacements resolve — `fn arm_keepalive` exists in `ccpa/carplayd/src/main.rs` and is called in `run_pairing_server` right after `set_read_timeout(30 s)`; `ControlServer::av_idle_ms()` exists in `crates/vendor/receiver/src/server.rs` and is consumed by the `WouldBlock` arm of `serve_connection` in `net.rs`. **Not yet on the truck** — the installed APK is `7c92000`, which predates it | No |

### Audio (A6–A9) — filed 2026-09-10 from the 2026-09-09 capture

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
the duck stays energy-gated because it is the only per-prompt signal NAV and ALERT have), and
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
| N17 | `ControlServer::av_idle_ms()` is not exported over JNI, so Kotlin's `handleNative` breaks the control connection on a 30 s read timeout even while A/V is flowing, where the reference `continue`s | **BLOCKED by the static-Rust constraint (2026-09-10)** — the fix is an export from `crates/vendor/receiver`, which is out of scope. Either it stays open as a known divergence from the reference, or a Kotlin-side proxy is found. Do not open the crate for it | The `NOT YET IMPLEMENTED` block in `CarPlayRx.kt` (search `arm_keepalive`) names two gaps and cites rows N13–N16, but only the keepalive half had a row. This is the other half. `pub fn av_idle_ms` exists in `crates/vendor/receiver/src/server.rs`; the `WouldBlock` arm of `serve_connection` (`net.rs`) is the reference behaviour | No |
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
| 3.4/3.5 | Split HOST_GONE emission; `F_REPLAY` bit2 semantics | **UNVERIFIED** | could not locate the source module (not under `crates/` or `tools/` in this checkout) | No |
| R6.5 | TTL-gated deploy trial / dead-man before flashing a daemon | Process step, not code — treat as standing procedure, not a landed/open item | — | — |
| R6.6.27–28 | Rotate AP passphrase; rotate `edSeed`/setup code fallback | **OPEN** | Corrected 2026-09-09: the `crates/vendor/pairing/src/srp.rs` `b"3939"` citation was a false positive — every occurrence (`:205,218,226,231,232`) sits inside `#[cfg(test)] mod tests` (opens `:195`, EOF `:236`), a test fixture never read at runtime, not a shippable credential; dropped as evidence. The real setup-code fallback is `const PI_FALLBACK` in `ccpa/carplayd/src/main.rs` (read by the `CARPLAY_PI` `get_or_init` fallback) — the old `crates/vendor/rx-connect/src/main.rs:29` pointer was stale, that crate no longer exists in this checkout and appears to have been merged into `carplayd`. `edSeed` rotation remains open per T6.5 above | No |
| R6.6.29 | Scrub tracked credential files, widen `RE_WPA` redactor | **OPEN** | `ccpa_custom/pi/evidence/hostapd_5g.conf` still tracked; no box-side `RE_WPA` symbol found in `ccpa_custom`. Scoping note (2026-09-09): an `RE_WPA` symbol does exist, but in this repo as `RE_WPA` in `logging/LogExport.kt` — that is app-side logcat redaction, not the box-side credential-file scrub this item is about, and must not be mistaken for coverage of it | No |

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
  does NOT cover re-seeding the per-Activity observers on an Activity recreate —
  `MainActivity.sessionUp` (`@Volatile` field, `MainActivity.kt`), `VehicleStateWatcher` (`MainActivity.vehicle`, `by
  lazy` per-Activity instance), and the supervisor's phase tracking are all still Activity-owned state
  with no relocation path of their own. That is a separate defect class; do not read T2.2 landing as
  closing it.
- **T5.1 — env-var config → `nativeInit` config.** Still the `−16720` desync class: `/info` is
  static-baked while `CARPLAY_SESSION_MGMT` etc. are env vars set in `JNI_OnLoad`
  (the `std::env::set_var(...)` block, `lib.rs`). Fix: one explicit config struct through `nativeInit`; generate `/info` in Rust
  from the same config; keep both paths with a startup byte-compare assert for ≥1 milestone before
  deleting the env path.
- **T6.4 — gate debug dumps.** `CARPLAY_SCREEN_DUMP`/`CARPLAY_SETUP_DUMP`/`.verbose(true)` are always
  on (the `CARPLAY_SCREEN_DUMP` `set_var` in `JNI_OnLoad`, `lib.rs`) → unbounded disk growth in production. Gate behind `BuildConfig.DEBUG`.
- **T6.5 — rotate `edSeed`.** Still `(it*31+7).toByte()` (the `edSeed` constructor default, `CarPlayRx.kt`) — deterministic, derivable
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
