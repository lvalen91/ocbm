# Runtime State Snapshot — 2026-07-10 (P0 lifecycle hardening deployed)

Point-in-time capture of the full CCPA + host-app runtime after deploying and validating P0 of the
lifecycle-hardening plan (docs/carplay/02_SESSION_LIFECYCLE.md). Captured over the UART console + the Mac host logs while a live
CarPlay session was streaming. All figures are as-observed; box wall-clock is unreliable (no RTC), so
uptime is the reference clock.

## 0. Headline
Clean cold session established under the new P0 supervisor and is streaming with **0 decrypt failures**.
Lifecycle state = `STREAMING`, `session_healthy=1`. P0 tasks #22/#23/#25/#26 validated on hardware; #24
(phone_reset ladder) deployed but its escalation actions are not yet fault-injected and have a known gap
(projection-failure loop not wired to the ladder — see §10).

## 1. Deployed P0 artifacts (box `/script`, md5-verified against repo `tools/`)
| File | md5 | role |
|---|---|---|
| session_supervisor.sh | `54ad0c198fde84cc33baa3ff1ed23376` | health gate, flap/stall detect, escalation ladder, state file, deferred-op apply |
| phone_reset.sh | `386b8809335ef7f3b73f608d6ac9951d` | automated power-cycle equivalent (ci_hdrc.0 OTG baseline) |
| peer_store.sh | `010211d53288aeb93406acb65172cd5e` | idle-gated pairing-store mediator |
| carplay-status.sh | `7f69fc3402ce0cbdda8bf48023e9ca00` | one-glance lifecycle reader |
Rollback: `/script/session_supervisor.sh.p0bak` (3485 B, the pre-P0 supervisor).

## 2. Box system
- uptime: 4 min (clean reboot into the P0 supervisor via the boot chain)
- kernel: `3.14.52+g94d07bb`
- load average: `1.41, 0.90, 0.39` — single-core i.MX6UL busy forwarding encrypted A/V (CPU-bound, as expected)
- memory: total **123460 kB**, used **7048 kB**, free **110468 kB**, available **111872 kB** → NOT memory-bound

## 3. Processes + RSS (the whole CarPlay stack)
| pid | proc | RSS | launched by |
|---|---|---|---|
| 124 | `/usr/sbin/ocbmd` | 432 kB | ocbm_boot.sh (boot) |
| 125 | `session_supervisor.sh` | (shell) | ocbm_boot.sh (boot) |
| 508 | `iap2d /dev/android_iap2` | 188 kB | projection_up.sh (on ARM) |
| 556 | `airplayd` | 544 kB | supervisor arm() |
| 557 | `rx_connect` | 844 kB | supervisor arm() |
Total daemon RSS ≈ 2 MB.

## 4. Lifecycle state (P0 signals)
```
host_present=1   session_healthy=1
/tmp/carplay_state:
  phase=STREAMING
  host_present=1
  armed=1 healthy=1 stuck=0
  paired=1 record=1
  flaps=0 stuck_fails=0 l1=0 l2=0 fails=0
  reason=
```
`flaps` cleared 1→0 after the session held established ≥15 s (the #23 counter-reset-on-established rule).

## 5. USB / gadget / network
- iPhone is role-switched to USB **host**; box presents the gadget: `state=CONFIGURED functions=iap2,ncm`
  (so no `05ac` device on the box bus — correct for an active projection).
- `ncm0`: `state UP,LOWER_UP`, IPv6 link-local `fe80::c08e:30ff:fe52:e9a3/64` (CarPlay control/data ride
  IPv6 link-local over ncm0).

## 6. Pairing
- `/etc/carplay_peers.bin` present, **69 bytes** = 1 known device (pair-verify fast path in use).

## 7. Logs
**supervisor.log (full — the clean P0 establishment):**
```
[sup] up; IDLE — gating the CarPlay session on /tmp/host_present (waiting for a host app)
[sup] host PRESENT -> go projection-ready + ARM
[proj] iPhone at /dev/bus/usb/001/002 — iAP2 handshake → projection
[proj] IDENTIFIED — projection-ready; bringing ncm0 up
[sup] ARMED (airplayd + rx_connect) — awaiting pair-verify -> RECORD
[sup] milestone: pair-verify OK — control encrypted (RECORD grace 30s)
[sup] milestone: RECORD — session ESTABLISHED (health=1)
```
**ocbmd.log:** `SUBSCRIBE (64 B config)` → `host PRESENT`.
**iap2d.log:** `SYN-ACK — link up` → `AuthSuccess` → `RX 0x1D02 Identified` → `RX 0x4E0A/0x4E0B Identified` (stable; no "host gone").
**airplayd.log (milestones):** pair established → `SETUP phase2 screen(110)` + `SETUP phase2 audio(100) fmt=0x8000 48000Hz 2ch Pcm audioType="media"` → `fwd-enc: handed video/media key` → `forwarding ENCRYPTED frames/RTP`. `/command` channel receiving `disableBluetooth` (115 B) + `modesChanged` (287 B) — logged, not yet handled (task #19).
**rx_connect.log:** `resolved _carplay-ctrl … -> fe80::… , 169.254.208.240 scope=ncm0(3)`; the trailing IPv4
`169.254…` `connect-out failed: Network unreachable (os error 101)` — **harmless** (IPv6 link-local path
already carries the control connection; documented behavior).

## 8. Host (Mac) app
- process pid 85127; window "CarLink" size **1200×482**
- FileLogger (`carlink_2026-07-09_233400_85127.log`): `OCBM host connected — SUBSCRIBE sent` →
  `received audio key` / `received video key (40 B)` → `H264 Format updated from SPS/PPS — 800×480`
- decrypt tally: **video ok=245 fail=0, audio ok=37822 fail=0** (audio steadily climbing = media playing;
  video count near-static = static CarPlay screen content; zero failures throughout)
- coded resolution: **800×480** (unchanged — task #21 `updateDisplayPanels` not yet implemented)

## 9. P0 validation status
| Task | Status | Evidence |
|---|---|---|
| #22 health signal | ✅ validated | `milestone: RECORD` → `session_healthy=1` |
| #23 flap/stall detect + teardown fix | ✅ validated | `paired/record` latched; `flaps` cleared after 15 s; no false stalls |
| #25 idle-gate mutation | ✅ validated | `peer_store.sh clear` refused while present=1 (exit 1, store intact) |
| #26 state file + reader | ✅ validated | `carplay_state` + `carplay-status` output correct |
| #24 phone_reset ladder | ⚠️ deployed | detection correct, no false triggers; **L1/L2 actions not fault-injected; projection-failure gap (§10)** |

## 10. Known open items / gaps
1. **#24 ladder blind spot (found during deploy):** a pre-ARM `projection_up.sh` failure (e.g. iPhone not
   enumerating as `05ac` after a mid-session disturbance) loops `projection failed — retry` forever with
   **no escalation** — the ladder triggers on establishment-stall (needs `armed=1`) and presence-flap,
   neither of which a projection failure hits, even though `phone_reset` would fix it. This limbo forced a
   manual reboot during the P0 deploy. Fix: count consecutive projection failures → L1 phone_reset.
2. **#24 escalation actions unproven on hardware** — L1/L2 need a controlled fault-injection to validate.
3. **Deploying under a live session is disruptive** — the warm supervisor-restart left the USB/projection
   state in limbo; a clean reboot was required. Future supervisor swaps should happen at idle (present=0).
4. **Resolution pinned at 800×480** — task #21 (`updateDisplayPanels`) outstanding.
5. **`/command` not handled** — `disableBluetooth`/`modesChanged` logged only (task #19).
6. Cosmetic: `carplay-status` ncm0 line shows a trailing `\` from `ip -o` output; harmless.

## 11. Rollback procedure
`cp /script/session_supervisor.sh.p0bak /script/session_supervisor.sh` then restart the supervisor (or
reboot). phone_reset.sh / peer_store.sh / carplay-status.sh are additive (new files); removing them and
reverting the supervisor fully restores the pre-P0 behavior.
