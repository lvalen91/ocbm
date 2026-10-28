# 06 — Bring-up runbook (device-proven through 2026-08-27; build/install/auto-claim/UI sections corrected 2026-10-08)

What is proven on hardware, and the exact commands to repeat it. Reading order for context:
[`04_SYSTEM_MODEL.md`](04_SYSTEM_MODEL.md) → [`05_SESSION_FLOW.md`](05_SESSION_FLOW.md) → this file.

---

## 1. What is proven

| Step | Status | Evidence |
|---|---|---|
| Ordinary Android app claims `0x1314:0x2d00` | done | `claimed interface 0 (class 0xff)` |
| OCBM framing byte-compatible with the box | done | `CT_HELLO_ACK v1 caps=0x0000003f` |
| `CT_SETTIME` applied | done | ack `status=0 (applied)` |
| MFi certificate over `CH_MFI` | done | 945 bytes, `30 82 03 ad 06 09 2a 86 48 86 f7 0d 01 07 02` (PKCS#7 signedData) |
| MFi signature over `CH_MFI` | done | 128 bytes (RSA-1024) |
| `CH_MGMT` identity snapshot | done | `<box BT name A>`, serial `<box serial>` |
| `CT_SUBSCRIBE` wakes the box radios | done | `host_present=1`, `hci0 UP RUNNING PSCAN ISCAN` |
| Box raises no SoftAP (`wifi_ap: false`) | done | no `hostapd`, no `wlan0`, moal/mlan not loaded |
| Box spawns no A/V layer | done | `airplayd absent` via `AIRPLAYD_BIN=/bin/true` |
| iPhone completes the full BT handshake | done | see §2 |
| App-supplied hotspot creds reach the `0x5703` source | done | `/etc/hostapd.conf` regenerated from the OCBM config |
| `0x5703` hands the iPhone the VEHICLE hotspot | done | `replying 0x5703 (ssid="<vehicle SSID>" sec=Wpa2OrWpa3Personal)` |
| iPhone joins `br0` | done | `num_connected_clients=1`, `<IPHONE_IP>` / `<MAC-1>` |
| iPhone advertises `_carplay-ctrl._tcp` on br0 | done | `<phone name>._carplay-ctrl._tcp.local` |
| App has direct unicast reach to the phone | done | ICMP true, TCP 62078 + 49152 OPEN |
| App is the AirPlay endpoint, phone dials in | done | `POST /pair-setup` inbound — §6.1 |
| SRP-6a pair-setup verified against a real iPhone | done | M4/M5 reached — §6.2 |
| Rust core cross-compiles and runs on-unit | done | §6.3 |
| Full A/V session (HEVC + AAC, both stream SETUPs) | done | §7.1 |
| Untethered fault capture (no laptop attached) | done | §8 |

Reproduced on the real truck (`gminfo37`, serial `<unit serial>`), not only the emulator.

**The keystone is proven.** `CH_MFI` works from an ordinary app UID — the single most load-bearing
unknown in the architecture.

## 2. The captured BT handshake

Driven entirely by the app's `CT_SUBSCRIBE`, with the box raising no access point:

```
[ssp-agent] loaded 1 persisted link key(s) — bonded phones can reconnect without re-pair
[ssp-agent] DEVICE_CONNECTED <MAC-2>
[sdp-client] iAP2 RFCOMM channel on the phone = 1
[bt-driver] SYN-ACK -- link up
[bt-driver] RX 0xAA00 -> CertSent          <- MFi chip call #1 (local i2c, box-side)
[bt-driver] RX 0xAA02 -> SignSent          <- MFi chip call #2
[bt-driver] RX 0xAA05 -> Authenticated
[bt-driver] TX 0x1D01 IdentificationInformation (301 B, wireless transport)
[bt-driver] RX 0x1D02 -> Identified
[bt-driver] RX 0x5702 RequestAccessoryWiFiConfig -> replying 0x5703 (ssid=... ch=36 sec=Wpa2OrWpa3Personal)
```

iOS retries `0x5702` (observed twice per session) — the handler must be idempotent. The
accessory-initiated reconnect path also completes.

---

## 3. Box-side changes (all reversible)

Two files on the adapter. Nothing else was modified.

| File | Change | Restore |
|---|---|---|
| `/script/session_supervisor.sh` | `wifi_ap:` gate, `apply_host_wifi_creds()`, `AV_SUPPRESS` | `git checkout tools/session_supervisor.sh` in `ccpa_custom`, then `tools/uart_push.sh` |
| `/etc/hostapd.conf` | ssid / passphrase / channel regenerated from the app's OCBM config | `cp /etc/hostapd.conf.stock /etc/hostapd.conf` (backup made automatically) |

Both default to stock when the keys are absent, so an unmodified app sees unmodified behaviour.

The credential path deliberately avoids a Rust rebuild: `wifi_handoff::read_hostapd_ap_config()` reads
`/etc/hostapd.conf` unconditionally, so writing that file is enough to change what `0x5703` sends. This
is only safe because `wifi_ap: false` means no `hostapd` ever runs from it.

---

## 4. Commands

### Build + install

```bash
bash tools/test.sh                 # Tier-0: tree-artifact guard, cargo tests, JNI cross-build, 46 JVM test classes, docs budget, proto check
bash tools/build_apk.sh            # release flavor;  GMCCPA_BENCH=1 bash tools/build_apk.sh  = bench flavor
# -> apk/gmccpa-debug-<STAMP>[-bench].apk. The build prints the full path AND the install line; copy it.
```

**Build output never lives in the tree (owner rule, enforced 2026-10-08).** `~/Documents` is iCloud-synced
and its conflict copies poison build trees (`ccpa_custom/CLAUDE.md`). Both scripts route every cargo call
through `CARGO_TARGET_DIR` under `~/.cache/cargo-targets/` (the receiver/pairing `cargo test` runs included —
until 2026-10-08 those built ≈1.1 GB in-tree), the APK work dir is a `mktemp -d`, `apk/` is a symlink into
`~/.cache`, and python runs with `PYTHONDONTWRITEBYTECODE=1`. The gate's first step,
`tools/tree_artifact_guard.sh`, FAILS and lists any real `target`/`build`/`.gradle`/`__pycache__`/`.build`/
`node_modules` directory under `host/gm_ccpa` or `crates/` (symlinks into `~/.cache` pass); set
`GMCCPA_ALLOW_TREE_ARTIFACTS=1` only to run the rest of the gate while cleaning such a directory up.

**Version stamp (corrected 2026-10-08; `build_apk.sh` header, V6/Q009).** `versionCode` is the commit count
(`git rev-list --count HEAD`), `versionName` is `4.0+<STAMP>` (+`-bench` for the bench flavor), and `STAMP` is
`<sha>` for a clean tree or `<sha>-dirty.<h8>` for a dirty one, where `h8` is the first 8 hex digits of a
sha256 over `git diff HEAD --binary` plus every untracked, non-ignored file. Untracked files count toward
dirty; a git failure is FATAL, never read as clean. The same STAMP is the APK filename and
`assets/build_sha`, which `IDENTITY` prints. Dirty builds on one commit share a `versionCode`, so identity
between them rides on `versionName`/`build_sha`, never on the code. The old "versionCode is pinned at 7"
statements are gone: older artifacts (all code 7) cannot be installed over a newer build with `-r`; rolling
back needs `adb uninstall -k android.car.usb.handler` first (`-k` keeps the data so the pairing survives). There is still NO
"latest" symlink (removed 2026-09-10): the STAMP in the filename is the only tie between a binary and its sources.

**Install procedure (owner-set, 2026-10-06).** Confirm `adb shell am get-current-user` prints `10` first;
stop and report if it does not. Then:

```bash
adb install --user 10 -g -r -i com.android.vending apk/gmccpa-debug-<STAMP>.apk
```

`-i com.android.vending` keeps the in-motion path eligible; `-g` grants the runtime permissions (including
`SYSTEM_ALERT_WINDOW`, which is `development`); `--user 10` is mandatory (without it adb installs for every
user: the package was once found installed, stopped and never launched, for user 0). **Any uninstall followed by
a reinstall must have a 30 second wait in between** (owner, 2026-10-06). A plain uninstall also wipes
`carplay_peers.bin`, the per-install accessory seed and the saved hotspot details, so the iPhone must re-pair and
the hotspot dialog is shown again; use `uninstall -k` only when you want to keep them.

### USB permission grant (re-run after every uninstall + reinstall)

The USB permission dialog for the adapter is removed by a persistent grant, written once from the shell uid
(which holds `MANAGE_USB`) through an `app_process` helper — mechanism, evidence and limits are in
`docs/host/01_ANDROID_AND_AAOS.md` ("Bench verification of the persistent USB grant"). The helper is not in
`tools/` yet (it lives in session scratch pending the owner's approval to land it); its contract is
`CLASSPATH=/data/local/tmp/usbgrant.dex app_process /system/bin UsbGrant <list|grant|revoke|status> [pkg] [userId]`,
e.g. `… UsbGrant grant android.car.usb.handler 10` with the adapter attached. The entry is keyed on the adapter's
descriptor strings and serial plus the app's uid and the user, so: re-run after any uninstall/reinstall (new
uid; the uid did change across this session's reinstalls) and after a box swap; a plain `install -r` keeps
it (bench-confirmed 2026-10-08, rc2 → `230eaa7`: `is_granted=true` after the upgrade — while READ_LOGS was
lost by the same `-r` and needed §8.1 again). `revoke` stores `is_granted=false` rather than deleting — run
`grant` again to undo. Without the grant
the auto-claim logs `SKIP_NO_GRANT` and never prompts; Start then asks as it always did.

**Changed 2026-10-15 (owner request):** the app installs as package `android.car.usb.handler` again (it was
`zeno.gmccpa` 2026-09-08..10-15, after the original fixed-handler squat was reverted). Every
`am`/`pm`/`appops`/`dumpsys` command below targets that package name; source classes are still
`zeno.gmccpa.*`, so activity components are `android.car.usb.handler/zeno.gmccpa.<Activity>` — the short
`/.MainActivity` form resolves against `android.car.usb.handler` and fails. The manifest's
`android.car.usb.handler.UsbHostManagementActivity` alias is the component GM's fixed handler starts on each
attach, after granting this uid; whether that per-attach grant now makes the persistent grant above redundant
is not bench-verified — keep running `UsbGrant grant` after a reinstall. Moving from `zeno.gmccpa` is an
uninstall of the old package (otherwise two copies coexist, each with its own `AdapterAttachReceiver` auto-claim), not an `install -r`.

A user-0 install is not the driver's user and never claims (`AutoClaim.kt` step 1). **Corrected
2026-10-08:** this paragraph used to say to expect the one-time permission dialog per device. Without the
persistent grant above that is still what happens (the ordinary attach resolver is never launched on this
unit, so the dialog comes from the app's own claim loop); with it there is no dialog. The
`android.car.usb.handler` squat never avoided the dialog either (corrected 2026-09-10), so there is nothing to
go back to.

### Auto-claim with the app closed (plan v5.9 D6, 2026-10-08)

The adapter is claimed without opening the app on three triggers, each logged as
`auto-claim (<origin>): <DECISION> — <why>`: `usb-attach` (the `AdapterAttachReceiver` manifest
receiver — hot plug or box reboot), `boot` (BOOT_COMPLETED) and `media-browser bind` (the media host
binding `CarPlayMediaBrowserService`, ~+7 s at boot when CCPA is the source). No dialog is ever raised
from these: without the persistent USB grant the decision is `SKIP_NO_GRANT` and Start asks as before.
While the adapter is linked one ongoing notification ("GM CCPA status" channel) shows the state; it
carries no names or addresses. **`am force-stop` does NOT keep the app closed on this unit** (bench 2026-10-08,
`6c95684`): the system re-binds the media browser within ≈1 s, the auto-claim re-claims the adapter and CarPlay
is live again ≈4 s later. To hold the app down use the in-app Stop (below), or the `carplay_stop` + `ocbm_stop`
verbs (the process stays alive, nothing re-claims; no Stop-until-boot flag, so the next launcher open starts it
— observed ≈4.9 s from reopen to CarPlay live). An on-screen **Stop** suppresses the auto-claim until the next head-unit
boot (`stop flag: boot identity = boot_count|boot_id|none`); Start or Restart session clears it.

**SYSTEM_ALERT_WINDOW rule** (it admits the background FGS start and the CarPlay screen's background
launch): effective = appop `allow`, OR appop `default` AND the permission granted. `install -g` grants
it (`development`), so nothing extra is needed after an install with `-g`. Never set the appop to
`ignore`/`deny` (that overrides the grant); `appops set --user 10 android.car.usb.handler SYSTEM_ALERT_WINDOW
allow` is optional and lost on uninstall. Preflight (read-only, no serials printed):

```bash
adb shell appops get --user 10 android.car.usb.handler SYSTEM_ALERT_WINDOW
adb shell dumpsys package android.car.usb.handler | grep -c 'android.permission.SYSTEM_ALERT_WINDOW: granted=true'
adb shell cmd package query-receivers --user 10 -a android.hardware.usb.action.USB_DEVICE_ATTACHED | grep -c android.car.usb.handler
adb logcat -d -b all | grep -a -E 'auto-claim \(|link hold|stop flag' | tail -20
```

"Adapter reconnecting" with no adapter present is by design: a detach arms a relink bounded at 10 minutes
(`OcbmProbe.kt:494`, `:591`), then the link hold is released (`SessionCore.kt:1495`). Fix round finished
2026-10-08.

The bench steps (B-D6-0 … B-D6-5: replug with the app closed, box reboot, cold boot, Stop for the
boot, headless missing credentials via the bench-only `finish_launcher` verb) and their pass values
are in the plan's D6 "Bench" section; `grep -a` is mandatory on those captures (binary bytes stop a
plain grep).

### Run (scriptable — no tapping)

```bash
# framing + state machine, no hardware needed at all
adb shell am start -n android.car.usb.handler/zeno.gmccpa.MainActivity --es run ocbm_selftest

# the real link. NOTE the nested quotes: an SSID with a space is otherwise split by the remote
# shell and every later --es extra is silently swallowed by `am`.
adb shell "am start -n android.car.usb.handler/zeno.gmccpa.MainActivity --es run ocbm_link \
  --es ssid '<vehicle SSID>' --es pass '<passphrase>'"

adb logcat -d -s NETPROBE
```

No channel is passed, on purpose (owner, 2026-10-07): the app sends no `wifi_channel`, so the box
keeps its stock hostapd channel. An `--es chan` extra is not read at all (plan v5.9 UI-2, D015, 2026-10-08:
the "ignored" log line went too, and the next credentials Save purges a channel an older build stored). With no
saved hotspot credentials and no `--es ssid/pass`, a start or USB attach opens the credentials dialog
instead of linking (OD-7A; there are no compiled-in credentials since 2026-10-07). `--es ssid/pass` apply to
that run only and are never saved; only the dialog's Save writes `HotspotPrefs` (`HotspotPrefs.kt`).

Other `--es run` verbs (`MainActivity.handleRunExtra`): `full` (BT handoff + Wi-Fi endpoint together —
the real end-to-end sequence, §7.1), `carplay_ui`, `mdns_self`, `carplay_stop`, `ocbm_state`, `ocbm_disconnect`, `ocbm_forget`, `ocbm_stop`,
`capture_status`, `capture_whole_os`, `capture_own`, `export_log`, `export_log_raw` (§8.2). A `run` verb executes
once per delivery: a recreate of the launcher (configuration change, process death restored from recents, the
task re-selected from history) re-delivers the original intent but does NOT replay the verb — it auto-starts
instead, and the retained intent is stripped of `run`/`ssid`/`pass`/`mac` (D-F2, 2026-10-08, `IntentReplay`).

`mic_inject` (plan v5.9 MIC, owner-approved 2026-10-07) exists only in a **bench** build
(`GMCCPA_BENCH=1 tools/build_apk.sh`, versionName `…-bench`); a release build answers
`mic_inject: not in this build (release)`. It streams a pushed 16 kHz mono S16LE WAV into the open
uplink gate in place of the vehicle mic (lead silence, the clip, tail silence, then the real mic),
paced by AudioRecord. Enable per test with `adb shell setprop debug.gmccpa.mic_inject 1` (back to `0`
after; props reset on reboot) — MainActivity is exported, so while it is 1 any installed app could
send the verb. Arguments: `--es file <path>|-|none`, `--ei lead_ms` (700), `--ei tail_ms` (1500),
`--ei expire_s` (60). Make the clip with `say -o p.wav --file-format=WAVE --data-format=LEI16@16000
--channels=1 "<text>"`, `adb push` it to `/data/local/tmp/gmccpa_mic.wav` and `chmod 644` it; if the app
cannot read that path, `--es file -` takes it over `toybox nc 127.0.0.1 9119` within 30 s instead. `--es file none` clears an armed clip and is honoured even while the property is 0 (it can only remove a
clip; `bench/BenchHooks.kt` KDoc).

`finish_launcher` (plan v5.9 D6, bench build only; a release build only logs) applies any
`--es ssid/pass` and then finishes MainActivity, so the headless auto-claim can be tested (BACK only
moves this singleTask launcher root to the back). Pass check: the log shows `launcher view detached —
the session core runs on headless`.

The passphrase cannot be read programmatically on the head unit; read it from Settings ▸ Hotspot
(`com.gm.hmi.connection`'s `WifiHotspotActivity`) and type it into the app's hotspot field, or pass it
as `--es pass`.

### Watch the box

**Signature: `uart_cmd.sh OUTFILE SECONDS 'command'`** — outfile first, then the capture window, then
the command.

```bash
cd ../..   # host/gm_ccpa -> ccpa_custom root; `cd ../ccpa_custom` does not exist (corrected 2026-09-09)
bash scratchpad/uart_cmd.sh /tmp/u1.txt 8 'tail -40 /tmp/wl.log'   # BT / iAP2 / handoff trace
tr -d '\r' < /tmp/u1.txt | tail -40
bash scratchpad/uart_cmd.sh /tmp/u2.txt 8 'grep -E "^(ssid|channel)=" /etc/hostapd.conf'
tr -d '\r' < /tmp/u2.txt
```

Send one or two short commands per call — long compound lines come back truncated or as a garbled
`/bin/sh: ???…: not found` — and strip `\r` from the capture before reading it. A call that returns only
a bare prompt usually means the command was too long, not that the box is wedged; re-send it shorter.

**Prefer OCBM over the UART whenever the adapter is on USB.** The UART is for recovery — a box that will
not enumerate, or a flash. For everything else the OCBM link already carries what you need, faster and
without a second cable — and the app now streams `/tmp/{bt,wlan,wl,supervisor,ocbmd,attach}.log` to
logcat over `CH_FILE` as `[box:<file>]`, so watching the box's logs live needs nothing extra:

| Want | Use |
|---|---|
| Run a command on the box | `python3 tools/ocbmcmd.py "<cmd>"` (one-shot, `CH_CONSOLE`) |
| An interactive root shell | `ocbm-host console` |
| Read a box file | `ocbm-host pull 1314 2d00 <remote> <local>` (`CH_FILE`, CRC-checked) |
| Deploy a binary or script | `tools/ocbm_push.sh <local> <remote> 755` |
| Watch the box's logs live | nothing — the app follows them and prints `[box:<file>]` to logcat |

`ocbmd` gives `CH_CONSOLE` its own output queue drained after audio and video, so a chatty `tail -f`
cannot starve a live A/V session.

**`tools/ocbm_push.sh` is not an install.** Its default set is `ocbmd` + `btd` and nothing
else — pushing `session_supervisor.sh` without `/script/radio_hal.sh` and `radio_detect.sh` leaves the
box with no radio bring-up and no error anywhere. This is exactly how Bluetooth died silently once
(`ccpa_custom` `docs/ops/06_CORRECTIONS_LEDGER.md` R-20W-5): a targeted push landed the supervisor without the radio
seam, `hciattach` failed with `Can't set line discipline: Invalid argument` underneath a flawless
firmware download, and the supervisor never reads the exit status of the detached script that reported
it. Use `ocbm_install.sh --full` for a real deploy, and after pushing the supervisor verify the radio
seam is present:

```bash
ls /script/radio_hal.sh /script/radio_detect.sh
```

---

## 5. Gotchas learned the hard way

1. **Do not cycle `host_present`.** Repeated app stop/start inside ~20 s trips the supervisor's flap
   detector, which escalates to an `ocbmd` restart and then to a full box reboot. Connect once and leave
   the link up; the 1 Hz heartbeat holds it.
2. **The bulk endpoints are IN `0x81` / OUT `0x01`**, not `0x83`/`0x02`. Discover the bulk pair by
   walking the interface; anything that hardcodes the addresses will fail.
3. **The phone must initiate the BR/EDR connection.** Outbound paging from the box returns
   `CONNECT_FAILED status=0x07` / `SDP query failed: Connection refused`. Successful reconnects all rode
   an ACL the phone opened. Trigger it from Settings ▸ General ▸ CarPlay on the iPhone.
4. **A stale hotspot poisons the next session.** After a session that handed out credentials for a
   network that no longer exists, forget that network on the iPhone before retrying.
5. **`pkill -f session_supervisor` kills the shell running it** (its own cmdline matches). Use an
   explicit PID, or the `[s]ession_supervisor` char-class trick the script itself uses.
6. **MFi can return status 1 transiently** while `btd` is mid-bring-up doing its own chip
   calls. Observed once as an immediate (~2 ms) failure on `copy_certificate` with the signature
   succeeding moments later; every subsequent attempt returned the full 945 bytes. Worth a soak loop
   before trusting it in the session path.
7. **A box reboot leaves `ocbmd` exiting for respawn** until a host reconfigures the gadget. If `HELLO`
   times out, check `/tmp/ocbmd.log` for `accessory POLLHUP/POLLERR` and confirm
   `/sys/class/android_usb_accessory/android0/state` reads `CONFIGURED`.
8. **logcat boots effectively dead on this unit.** All four ring buffers read 0 bytes at 64 KiB each.
   `setprop persist.log.tag V` and `logcat -G 16M` before concluding anything from silence — see §7.

---

## 6. The Wi-Fi endpoint: proven, plus the byte-level details that made it work

Ported faithfully from `crates/vendor/rx-connect/src/main.rs` rather than inferred. Verified on the
truck end to end (§6.1): advert with the Car bit, TXT set matching `rx-connect` exactly, SRV resolving to
a reachable br0 address, listener bound, browse + resolve of the phone's `_carplay-ctrl._tcp`,
`GET /ctrl-int/1/connect` accepted, and the phone dialing the control connection back in.

**The fix that made the connect-out work** — the single most valuable byte-level detail of the session:
`AirPlay-Receiver-Device-ID` is a decimal uint64, not a MAC string. `rx-connect`'s `mac_to_dec()` folds
the MAC; sending `<MAC-3>` makes iOS parse it base-10 as `0`, look up "receiver 0" among the
peers it browsed, find nothing, and have nothing to dial back to. The correct header for our identity is
`AirPlay-Receiver-Device-ID: <decimal deviceID>`. TXT `deviceid` and `/info` `deviceID` keep the colon MAC —
the asymmetry is deliberate.

Other corrections applied from the reference: `Host:` for an IPv6 peer must be bracketed with the zone
id stripped (the zone belongs to the socket); the CarPlay version family is **320.17**, not the
AirPlay-2 980.x; no `CSeq` on the connect-out (it is plain HTTP, not RTSP); retry the nudge 10×1 s and
clear dial memory on `ServiceRemoved`; never answer an unimplemented route with a bodyless 200 (a false
success on pair-verify M1 is worse than an error); echo the request's protocol version.

**Two platform facts that are not negotiable:**

1. **GM's own CarPlay receiver cannot be disabled from shell.** `pm disable-user` on
   `com.gm.domain.server.delayed/...CarPlayService` fails with `SecurityException: Shell cannot change
   component state` (a PERSISTENT guard). `:7000` stays held and GM's `CarPlay._airplay._tcp` keeps
   advertising from `192.168.5.1` — coexist with a distinct Bonjour instance name and a distinct port.
   `am start-service` cannot restart it either ("no service started"); it returns on reboot. `pm
   uninstall --user 10` does work, if that is ever the right tool.
2. **`NsdManager` cannot pin the advertised address** the way `rx-connect` does with `RX_ADDR`. Measured
   rather than assumed — the platform published `192.168.5.1` on br0, which is fine. If that ever
   changes, the fix is a controlled mDNS responder bound to br0, not NsdManager.

## 6.1 MILESTONE (2026-08-04): the iPhone opens the CarPlay control connection to the app

The full wireless-CarPlay chain closes, on the truck, into an ordinary non-privileged app:

```
[WiFi] Bonjour device added/updated
carManager_handlePendingAutoconnect: index = WiFi, for deviceID: <MAC-4>,
    pendingAutoconnectID: <MAC-4>
>>> INBOUND CONTROL CONNECTION from fe80::<phone-derived>%br0:58503
--- POST /pair-setup
```

Bluetooth handshake → MFi cert+sign → `0x5703` handoff to the vehicle's SoftAP → phone joins `br0` →
our advert indexed by iOS → connect command matched → the phone dials our app → first real CarPlay
request. Every step device-proven.

**Three things that made it work:**

1. **`NsdManager` for the advert.** It is the only thing observed to produce `[WiFi] Bonjour device
   added/updated` — i.e. to get us into iOS's Wi-Fi endpoint index, which the pending autoconnect
   resolves against. A hand-written responder emitting byte-perfect records (verified against dnslib,
   scapy and the `mdns-sd` crate) never produced an endpoint.
2. **Advertise before dialing.** `GET /ctrl-int/1/connect` does not make iOS connect; it sets a pending
   autoconnect resolved immediately against the already-populated index. Dialing in the same breath as
   advertising loses the race by milliseconds.
3. **Letting the cached endpoint age out** rather than forcing a fresh one mid-session.

**Pin the identity before a run, never vary it inside one.** iOS indexes the endpoint by `deviceid` and
holds it for the record TTL. Changing the advertised identity mid-session guarantees a spurious `No
matching endpoint found`, because the pending autoconnect carries the new id while the index still holds
the old one:

```
carManager_handlePendingAutoconnect: pendingAutoconnectID: <MAC-3> is expired (55.682 seconds old)
```

A diagnostic identity swap produced exactly this and looked like a real protocol failure. It was not.

**Caveats on this result:**
- GM's `CarplayService` was force-stopped during the successful run (`:7000` free). Coexistence is
  permanent in the shipping configuration and has since been re-tested (§6).
- The session stopped at `POST /pair-setup`, which was correct and expected at the time — see §6.2.

## 6.2 SRP-6a pair-setup verified against a real iPhone

> **Corrected 2026-10-08 (plan v5.9 D016):** this milestone was reached by a Kotlin SRP server, `pair/SrpServer.kt`,
> which (with `pair/Tlv8.kt` and the `handleStub` path) is deleted — `ls pair/` now shows only `NativeCore.kt`.
> The shipping receiver does pair-setup in the Rust core; with no core the connection is closed. The result
> below stands as the proof that SRP wire-compatibility with an iPhone holds; the Kotlin code does not exist.

`POST /pair-setup` was implemented in Kotlin (a port of `pairing/src/srp.rs` + `tlv.rs`) and driven by
the phone:

```
--- POST /pair-setup
pair-setup M1 -> M2 (salt 16B, B 384B, setup code 3939)
pair-setup M3: A=384B proof=64B — verifying SRP
*** MILESTONE: SRP-6a PROOF VERIFIED against a real iPhone — M4 going out
*** pair-setup M5 reached — the Ed25519 LTPK exchange
```

The phone accepting M4 and advancing to M5 is the proof — it only does that if our server proof matched
its own computation.

This closes a gap the reference itself flags as unclosable offline: `pairing/src/setup.rs` says "SRP
wire-compatibility with the iPhone's client is the single thing not provable offline (we have no
captured pair-setup)." It is now proven — including the documented HomeKit divergence that `H(g)` is
taken over the minimal generator byte `[5]`, not padded to the modulus length.

pair-setup needs no third-party crypto: SRP-6a is `BigInteger` + SHA-512, both on the platform at API 32.
M5/M6 (Ed25519 LTPK exchange) and pair-verify (X25519 + Ed25519) are the parts API 32 cannot do natively
— hence the Rust core (§6.3).

## 6.3 Build toolchain for the Rust core — proven end to end

Everything needed was already on the machine except two Rust components.

| Piece | Status |
|---|---|
| NDK | `30.0.15729638` in Android Studio's SDK, with `x86_64-linux-android32-clang` — the head unit's exact ABI and API level |
| Rust targets | `x86_64-linux-android` (head unit) + `aarch64-linux-android` (emulator) via rustup |
| `cargo-ndk` | 4.1.2 installed, but panics on this workspace — not needed, see below |

**Gotcha:** `/opt/homebrew/bin/cargo` (Homebrew's Rust) shadows rustup's shim and cannot add
cross-targets. Use `~/.cargo/bin/cargo`.

**Working recipe** (no `cargo-ndk`):

```bash
export PATH="$HOME/.cargo/bin:$PATH"
NDK=~/Library/Android/sdk/ndk/30.0.15729638/toolchains/llvm/prebuilt/darwin-x86_64/bin
export CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER="$NDK/x86_64-linux-android32-clang"
cargo build --release --target x86_64-linux-android
```

All six crates cross-compile clean, and a `cdylib` linking `receiver` + `pairing` produces:

```
liblinkprobe.so: ELF shared object, 64-bit LSB x86-64, for Android 32, built by NDK r30-beta2
```

Pushed to the head unit and verified. Three things this retired:

1. **The `ControlServer<'a>` lifetime is a non-issue.** The probe holds `&'static Identity` via
   `OnceLock` and links against the real crates. No fork.
2. **`x25519-dalek` and `ed25519-dalek` cross-compile** — the exact primitives API 32 denies, so the
   Rust core solves pairing and no third-party Java crypto provider is needed.
3. **The `eld-codec` trap is real and avoidable.** Building from the workspace root pulls it in via
   feature unification and fails on a missing `fdk-aac` header. Build `receiver` from its own directory
   with default features (`mic-uplink`) and it never enters the graph — same recipe `tools/test.sh`
   uses for the Tier-0 gate.

## 6.4 `/info` — generated from the reference, every number measured on the unit

`/info` is generated by `receiver::info::build_info()` via a small `infogen` tool, shipped as
`assets/info.bplist`, served as `application/x-apple-binary-plist`. Not hand-written: iOS validates this
capability set at RECORD and tears the session down if it is incomplete.

1879 bytes. Contains every key the reference's own regression test lists as validated at activation,
plus `modes` (without which the screen resource is `type=N/A` → `StartupFailed -17483`).

### The geometry is measured, not assumed

| Property | Measured on `gminfo37` | In `/info` |
|---|---|---|
| Panel | `2400x960`, `DD134IA-01B` | `widthPixels 2400`, `heightPixels 960` |
| Refresh | 60.001434 Hz | `maxFPS 60` |
| Density | 200, `192.911 x 193.523` dpi | `widthPhysical/heightPhysical 0` (matches the genuine CCPA) |
| App window | `2400x960` — full panel | — |
| System bar insets, normal | `left=189 top=118` — GM chrome overlays the panel | — |
| `mAppBounds`, normal | `Rect(189, 0 - 1605, 960)` = `1416x842`. Left 189 is the LeftBar; the right 795 is a separate navigation-class inset provider (GM's widget pane), NOT a bar. The 118 px TopCarSystemBar is status-class and does NOT reduce `mAppBounds` — top stays 0 — it purely overlays | second `viewAreas[]` entry |
| System bar insets, immersive | `0,0,0,0` | no `safeArea`, `viewAreas` lever off |

The immersive flag is a hard requirement **for the full-bleed view area only** — corrected 2026-09-08,
it used to say "hard requirement" unqualified. When CarPlay owns the whole panel, without
`IMMERSIVE_STICKY | FULLSCREEN | HIDE_NAVIGATION` the top-left `189x118` region is covered by GM's
chrome and CarPlay would put interactive UI under it.

In the SECOND view area the requirement inverts: the picture is cropped into GM's own app bounds, so
GM's chrome must be VISIBLE around it and immersive is dropped. See `04_SYSTEM_MODEL.md` §4d.

Two facts that make this work, both measured here and easy to get wrong:

- **AAOS insets the CONTENT, never the window.** `mBounds` stays the full `2400x960` and
  `mWindowingMode=fullscreen` in both states. GM's LeftBar and TopCarSystemBar are separate system-UI
  windows at a higher Z that OVERLAY ours — that is the default, not something the app arranges.
- **Only the three hide flags may toggle.** `LAYOUT_STABLE | LAYOUT_FULLSCREEN | LAYOUT_HIDE_NAVIGATION`
  must stay set permanently: they are what stop the decor padding our content. Clearing them alongside
  the hide bits moves the root to `(189,118)` and displaces the fixed `2400x960` surface — the picture
  lands at `377,236`, i.e. the LeftBar width and top-bar height added a second time.
  `FLAG_FULLSCREEN` (from `Theme.NoTitleBar.Fullscreen`) must toggle too: it suppresses the status bar
  independently of `systemUiVisibility`, so clearing only the sysui bits brings the LeftBar back and
  leaves TopCarSystemBar hidden.

### Audio matches the hardware exactly

Every output thread reports 48000 Hz, `AUDIO_FORMAT_PCM_16_BIT`, stereo (`front-left, front-right`);
`channels=0x2` everywhere, no multichannel anywhere:

- type 100 `default`/`telephony`/`speechRecognition` — AAC-ELD 16k mono in and out (the mic uplink)
- type 100 `alert`, type 101 `default` — AAC-ELD 48k stereo out
- type 102 `media` — AAC-LC 48k stereo, the high-latency path so music is not squeezed through the
  low-latency voice channel
- type 100/101 `compatibility` — PCM fallbacks

CarPlay's stereo-48k wire ceiling costs nothing here: the hardware is stereo 48k.

### Input: touch + media buttons only

`hidDevices` carries `CarLink Touchscreen` and `CarLink Media Buttons` (both with `displayUUID` matching
the display `uuid`, which iOS checks). D-pad, knob and telephony-button levers are off, so
`displays[].features` is `0x0A` rather than `0x1A`. Steering-wheel media buttons ride this HID path —
they do not depend on the metadata plane, so metadata can be deferred until after A/V is stable.

Regenerate with `infogen` if identity or geometry changes. `deviceid` / `pi` / `features` must stay
byte-equal to the Bonjour TXT or pair-verify fails.

## 7. A/V capture, the logcat trap, and a full session

**The head unit boots with logging effectively dead** — all four `logcat` ring buffers read 0 bytes,
each 64 KiB. Nothing from the app appears, which looks exactly like the app failing to run. Fix it at
the start of every session:

```bash
adb shell "setprop persist.log.tag V"   # revives logd
adb logcat -G 16M                       # 64 KiB is ~2 s of A/V logging
```

**Verifying a session without logs (the socket table never lies).** Derive the uid; never hardcode it —
it changes on every uninstall/reinstall.

```bash
APPID=$(adb shell dumpsys package android.car.usb.handler | sed -n 's/.*userId=\([0-9]*\).*/\1/p' | head -1)
SOCKUID=$((1000000 + APPID))     # user 10 => 10*100000 + appId; on user 0 it is just $APPID
adb shell "cat /proc/net/tcp6" | awk -v u="$SOCKUID" '$8==u && $4=="01"'   # 01 = ESTABLISHED
```

4–5 established sockets to the iPhone's `fe80::…%<if>` plus `POST /feedback` every 2 s = healthy session.
GM's own receiver owns `:7000` (`0x1B58`) under uid `1001000` — as of 2026-08-12 it listens again, so
coexistence is the live condition, not a hypothetical.

Do not leave `nc -l -p 9001` running from manual probing: it steals the seam port from the app and the
receiver's forwarder then connects to `nc` instead of the CarPlay screen's decoders.

---

## 7.1 Bringing up a full A/V session (the working procedure)

This is the sequence that produced the working session in
`~/Documents/carlink/old/gm_ccpa/evidence/session_2026-08-05/WORKING_SESSION.md` (the `evidence/`
archive lives in that standalone checkout, not under this tree — see `00_HANDOFF.md` §"Document
policy"). Assumes the CCPA adapter is on USB and the iPhone is paired to the vehicle over Bluetooth.

### 0. Revive logging first — silence is not evidence
```bash
adb shell "setprop persist.log.tag V"    # survives reboot; logcat boots effectively dead without it
adb logcat -G 16M                        # 64 KiB is ~2 s of A/V logging
```

### 1. Start the session (BT handoff + Wi-Fi receiver)
```bash
adb shell am force-stop android.car.usb.handler
adb logcat -c
adb shell "am start -n android.car.usb.handler/zeno.gmccpa.MainActivity --es run full \
           --es ssid '<vehicle SSID>' --es pass '<passphrase>'"
```
Wait for both stream SETUPs before opening the UI:
```bash
adb logcat -d -s NETPROBE | grep -E "SETUP phase2"
#   SETUP phase2 screen(110) ... -> dataPort NNNNN
#   SETUP phase2 audio(102) ... AacLc audioType="media" -> dataPort NNNNN
```

### 2. Open the CarPlay screen
```bash
adb shell am start -n android.car.usb.handler/zeno.gmccpa.MainActivity --es run carplay_ui
```
Expect, within a second:
```
[cpui] session up: seams listening on 127.0.0.1:9001 (HEVC) and :9002 (AAC-LC)
[cpui] renderer attached to Surface
[hevc] MediaCodec configured: video/hevc 2400x960 ... decoder=OMX.Intel.hw_vd.h265
[hevc] keyframe — decoding starts
[hevc] FIRST FRAME RENDERED
[aac ] FIRST AUDIO FRAME PLAYED
```

### 3. Watch it
```bash
adb logcat -d -s NETPROBE | grep -E "\[hevc \]|\[aac  \]|\[cpui \]"
```
`MainActivity` commands are safe during a live session — they detach the video renderer while audio and
the seams keep running; `carplay_ui` reattaches it. (The `av_sink`/`av_stats` diagnostic sink that
used to be listed here was removed 2026-09-11: it bound the same :9001/:9002 the real decoders need,
and the running counts it printed are in the `[hevc ]`/`[aac  ]` lines above.)
(`~/Documents/carlink/old/gm_ccpa/evidence/session_2026-08-05/WORKING_SESSION.md`
§5).

### 4. Restore the screen after anything backgrounds it
```bash
adb shell am start -n android.car.usb.handler/zeno.gmccpa.MainActivity --es run carplay_ui
```
Video returns in ~250 ms via the producer's re-dial + ForceKeyFrame. Audio is never interrupted.

**Do NOT** try `am start -n android.car.usb.handler/zeno.gmccpa.av.CarPlayActivity` — it is
`exported="false"` and `adb shell` (uid 2000) lacks `START_ANY_ACTIVITY` on this user build:
`SecurityException: … not exported from uid 1010123`. Always go through `MainActivity`.

### Verifying without logs (the socket table never lies)
```bash
APPID=$(adb shell dumpsys package android.car.usb.handler | sed -n 's/.*userId=\([0-9]*\).*/\1/p' | head -1)
adb shell "cat /proc/net/tcp6" | awk -v u="$((1000000 + APPID))" '$8==u && $4=="01"'   # 01 = ESTABLISHED
```
4–5 established sockets to the iPhone plus `POST /feedback` every 2 s = healthy session. Derive the uid
— do not hardcode it (§7): a stale literal shows an empty table on a healthy session.

### Troubleshooting

| Symptom | Cause | Action |
|---|---|---|
| App dials, phone answers `HTTP/1.1 200 OK`, no inbound on `:7011` | Stale iOS endpoint cache. iOS logs `carManager_handlePendingAutoconnect: No matching endpoint found for deviceID …` then `-6753 kConnectionErr` | Forget the vehicle on the iPhone (Settings → General → CarPlay) and re-pair over Bluetooth. Confirmed fix — not caused by GM's `:7000` |
| Black screen, session otherwise healthy | video seam bound but no frames | check `[hevc]` lines; `first frame decoded (0 B Annex-B)` means the sample-description unwrap regressed (`01_FINDINGS` §8b) |
| `bind :9001 … EADDRINUSE` | a stale CarPlayActivity generation or a manual `nc` holds the seam | the seam bind retries across TIME_WAIT on its own; kill any manual `nc -l -p 9001` |
| No `[hevc]`/`[aac ]` lines at all and no app logs | logcat is dead (§7) | `setprop persist.log.tag V`, `logcat -G 16M` |
| Nothing after a reboot | GM's `CarplayService` self-restarts and re-claims `:7000` | coexistence must be verified live each time — see §6 |

---

## 7.2 The credential-ordering trap, and the stale-resolve trap

Two failures found in one session. Neither produces an error at the point of the mistake; both present
much later as "CarPlay just doesn't start."

### Trap 1 — a credential-less `CT_SUBSCRIBE` pins `0x5703` to the box's stock SSID for the whole session

`CT_SUBSCRIBE` is the radio-wake edge, and the box applies host-supplied Wi-Fi credentials only inside
`wireless_up()` (`session_supervisor.sh` → `apply_host_wifi_creds`, defined at `:827` and called at
`:920` inside `wireless_up()` itself at `:863` — corrected 2026-09-09, it used to say the call was at
`:547`, which is unrelated escalation/health-check logic), which runs on the
`host_present` 0→1 edge.

> Subscribe once without credentials and the wireless stack comes up against the box's stock
> `/etc/hostapd.conf`. A later `CT_SUBSCRIBE` that does carry credentials will not re-apply them —
> `wireless_up` has already run.

Observed: the app sent a 153-byte config with `wifi_ssid: <vehicle SSID>`, the box logged nothing
wrong, and `/etc/hostapd.conf` still read `ssid=<box stock SSID>`. `0x5703` would have handed the iPhone an AP
that is never raised (`wifi_ap:false`), and per §5 gotcha 4 that dead network then poisons the next
attempt until it is forgotten on the phone.

**Fixed:** all three credential entry points (on-screen button, USB-attach, `--es run`) now go through
`MainActivity.applyHotspotFields()`, and `OcbmProbe` refuses to take the radio-wake edge with no SSID
rather than proceeding into an unrecoverable state.

**Recovery if you hit it:** a full `host_present` cycle — release the box for ≥20 s (the flap detector), then
relaunch with `--es ssid/--es pass`. Since 2026-10-08 (bench, `6c95684`) `am force-stop` alone cannot provide
that gap on this unit — the media-browser bind relaunches the app and re-claims within seconds — so use the
in-app Stop or the `ocbm_stop` verb to hold the box released for the 20 s. Verify before trusting it:

```bash
bash scratchpad/uart_cmd.sh /tmp/h.txt 11 'cat /etc/hostapd.conf'   # in ccpa_custom root (cd ../.. from here)
tr -d '\r' < /tmp/h.txt | grep -aE '^ssid=|^channel=|^wpa_passphrase='
```
Expect `ssid=<vehicle SSID>`. If it still says `<box stock SSID>`, the credentials did not reach
`wireless_up`.

### Trap 2 — `NsdManager` serves a stale peer address, and restarting discovery does not fix it

Symptom: the handoff succeeds, the phone joins `br0`, `_carplay-ctrl._tcp` resolves fine — and every
`GET /ctrl-int/1/connect` times out after 3000 ms with no response at all.

The address was dead. `NsdManager` kept answering with an iPhone IPv6 link-local that had rotated away:

```
$ adb shell ip neigh show dev br0
<IPHONE_IP>                 lladdr <MAC-5> REACHABLE   <- the phone, actually here
<IPHONE_LL6>      lladdr <MAC-5> STALE       <- and here
fe80::48f:dfe5:7179:6f60     INCOMPLETE                           <- what we were dialling
```

iOS confirmed the consequence from its own side — the advert was indexed correctly, so this is not a
discovery or identity fault:

```
[WiFi] Bonjour device added/updated
carManager_handlePendingAutoconnect: no pending autoconnections
```

Per §6.1 the connect-out is what sets the pending autoconnect. A dead dial address means it is never set,
so the phone never opens the control connection — with nothing in any log saying "wrong address."

Restarting discovery did not help: the platform answers a fresh resolve from the same cache, so it
returned the same dead address indefinitely. iOS rotates its IPv6 privacy address, so expect this to
recur. The cached PORT was stale too (`NsdManager` reported `57236`; the live SRV said `57292`) — assume
the whole cached record is suspect, not just the address.

**Fix: the wire is the authority, `NsdManager` is only the discovery trigger.**
`MdnsInspect.liveEndpoint()` queries the link directly (SRV, then A/AAAA for the SRV target) and
`CarPlayRx.dialCandidates()` puts those addresses first, re-querying at attempts 3 and 6 in case the peer
moves mid-retry. The cached pair is kept as the last candidate (the wire query can occasionally miss a
reply) but is never dialled first.

**Diagnosing it fast:** the neighbour table is the tell. `INCOMPLETE` against the address in the
`resolved '<peer>' -> …` log line means you are dialling a ghost:

```bash
adb shell ip neigh show dev br0
adb shell "ping6 -c 2 -W 2 <resolved-addr>%br0"    # "Address unreachable" = confirmed
```

---

## 8. Untethered capture

The two faults that survive every tethered session — the USB permission dialog appearing on roughly
half of adapter attaches (retired 2026-10-08 on a unit that carries the persistent grant, §4), and the iPhone
refusing to auto-reconnect until iOS forgets the box — only happen when no Mac is attached. The app captures
itself.

`logcat -d` dumps what is already in the ring buffer, so capture does not have to be running when the
fault happens, only within the ring-buffer window afterwards. Capture is armed
(idempotently) from every entry point — `MainActivity.onCreate`, `AutoClaim.trigger`, `BootReceiver`,
`CarPlayMediaBrowserService.onCreate`, and `UsbAttachActivity` when it runs — and drains the backlog before
it starts tailing (`LogCapture.start` call sites; corrected 2026-10-08: the attach trampoline is dormant on
this unit and is no longer the primary arm). Coverage does not depend on any one of them.

### 8.1 One-time bench grant

The declared default scope is whole-OS (`CapturePrefs`, so a granted unit never quietly captures less); without
`READ_LOGS` it degrades to own-process, which needs no permission and always works, and says so in the capture
header (corrected 2026-10-08). Whole-OS needs `READ_LOGS`,
which is `signature|privileged|development` — it can never be obtained by declaring it, but the
`development` flag means adb can grant it. The app declares it so the grant has something to land on;
`pm grant` rejects a permission the package does not request.

```bash
adb shell pm grant --user 10 android.car.usb.handler android.permission.READ_LOGS
adb shell am force-stop android.car.usb.handler       # NOT optional — see below
```

**The app's own on-screen/logcat suggestion is safe to copy again** (fixed 2026-09-09). It was not,
between 2026-09-08 and 2026-09-09: `LogCapture.kt`'s `GRANT_CMD`/`FORCE_STOP_CMD` (pre-fix file
byte-exact at commit `b049810` —
`git show b049810:host/gm_ccpa/netprobe_app/app/src/main/java/zeno/gmccpa/logging/LogCapture.kt` —
constants at `:163`/`:166`, printed at `:572`, `:608-609`, and into the capture-file header's
`degraded :` line at `:794`; the header's `app :` line at `:790` hard-coded the same literal directly)
survived the package-squat revert with the dead package `android.car.usb.handler` hard-coded, so a
copy-pasted `pm grant` targeted a package not installed on the unit and failed. They are now
`LogCapture.grantCmd(ctx)`/`forceStopCmd(ctx)`, built from the running app's `ctx.packageName`, so
the printed remediation and the capture-file header (`header()`'s `app :` and `degraded :` lines)
always name whatever package is actually installed. The commands above remain correct.

**The force-stop is load-bearing.** `READ_LOGS` maps to the supplementary gid `log`, and supplementary
gids are assigned at fork. An already-running process keeps reporting `GRANTED` from
`checkSelfPermission` while `logd` still filters it to its own uid — permission says yes, the kernel
says no. The engine detects exactly this — before the first capture file, it asks logd once
(`logcat -d -t 2000 -v threadtime -v uid`, retried for up to 15 s only while that probe cannot run or
returns no line) for a line of a uid other than ours; none seen = degrade — and it degrades the reported
scope and prints the force-stop command; believe the `effective` scope, never the granted flag.
(Corrected 2026-10-06: it used to look for a foreign *pid* in the captured stream for 15 s. That was
wrong: logd filters an unprivileged reader by uid, so lines from dead pids of our own uid are visible,
and the pid parser misread the `-v year -v UTC` format. A probe that cannot run logs `whole-OS capture
UNVERIFIED` and capture continues as requested; it never degrades without positive evidence.)

The grant survives reboots and `adb install -r`. It is lost only on a full uninstall.

### 8.2 Operating it

```bash
# status: which scope actually took, which buffers opened, bytes on disk, lines dropped
adb shell am start -n android.car.usb.handler/zeno.gmccpa.MainActivity --es run capture_status
adb shell am start -n android.car.usb.handler/zeno.gmccpa.MainActivity --es run capture_whole_os
adb shell am start -n android.car.usb.handler/zeno.gmccpa.MainActivity --es run capture_own
adb shell am start -n android.car.usb.handler/zeno.gmccpa.MainActivity --es run export_log
```

The launcher screen carries only Export: **Adapter & logs ▸ Export logs** (corrected 2026-10-08, plan v5.9
D003/D004: the Log Scope and Log Status pills were removed; scope and status are these verbs only). The
same sheet holds Restart adapter Bluetooth, Reboot adapter and Forget pairing, each confirmed, plus the
full "Last message" (the engineering sentence the screen's driver line no longer shows; it is also in
logcat as `ui: <STATE> — <detail>`). Recover was merged into Restart session (D001), and "Box Info" is gone
(MGMT_GET_INFO still runs on every link-up; `ocbm_state` reads the box state).

Export walks a ladder and reports which rung it used: SAF document picker → a mounted USB volume via
`getExternalFilesDirs()` → `MediaStore.Downloads` → a no-op that reports the path in `filesDir`. Some AAOS
builds ship without DocumentsUI; THIS unit has it (bench 2026-10-08, `230eaa7`): `export_log` opens the SAF
picker in its own task, which the CarPlay screen covers (it relaunches on MainActivity resume), so the export
sits pending until the app is brought forward — the picker's cancel was processed ≈43 s later, after which the
ladder fell through to MediaStore Downloads. Read that artifact with
`adb shell content read --user 10 --uri content://media/external_primary/downloads/<id>`; `adb pull` of the
Download path is permission-denied for the shell. `run-as` is blocked on this unit, so the ladder is the only
retrieval path that does not need adb.

**Exports are redacted by default.** A whole-OS capture from this unit contains the vehicle hotspot
passphrase, the iPhone's BR/EDR MAC (in the box's `[box:…]` log lines and the `MGMT_INFO` bonded-device
snapshot — `CT_PHONE_IDENT.deviceID` stays EMPTY in this role, the box never sees SETUP, F-C8), the adapter's
serial and derived `CarLink-xxxx` name, the VIN and GPS; an export lands on a removable stick and leaves the
vehicle. `export_log_raw` opts out and is a separate verb on purpose. The redactor's rules and its known gaps
are listed in `logging/Redactor.kt`'s class KDoc (corrected 2026-10-08; they moved out of `LogExport.kt` in
v5.8 V7) — read them before trusting an export.

### 8.3 What to grep

**Start from a plain `adb logcat > logcat.log`.** Capture everything, filter afterwards — the
unfiltered file is what tells you the fault was NOT the app, and that is worth more than a tidy one.

```bash
grep NETPROBE logcat.log                                    # the app's own narrative
PID=$(grep -m1 'IDENTITY pid=' logcat.log | grep -o 'pid=[0-9]*' | cut -d= -f2)
awk -v p="$PID" '$3==p' logcat.log                          # + every framework line the app caused
grep -E 'EXPECTED-MISSING|EXPECTED-LATE' logcat.log         # steps that did not happen, or nearly
grep '## STATUS' logcat.log                                 # standing state, nearest block to any offset
grep ' E NETPROBE' logcat.log                               # faults — E means fault since 6c6ec79
grep -E 'iPhone DETECTED|INBOUND CONTROL|MILESTONE' logcat.log
```

**Both filters are needed, and the second is the non-obvious one.** In the 2026-09-09 capture the
app's PID emitted ~250 lines under OTHER tags — `CCodec` 68, `CCodecConfig` 55, `CCodecBuffers` 45,
`BufferQueueProducer` 17, `MediaCodec` 11, `ViewRootImpl[CarPlayActivity]` 8, `SurfaceUtils` 7 —
and those are exactly the lines that separate an app decode fault from a platform one. `grep
NETPROBE` throws all of them away. The `IDENTITY` anchor exists so the PID filter is constructible;
before `6c6ec79` the PID reached logcat only on the USB-attach path, so a launcher start left
nothing to key on.

`IDENTITY` also carries the build SHA, stamped into `assets/build_sha` at build time — the SHA is in
the APK filename and `pm install` discards it, so without the asset a capture cannot be tied to the
sources that produced it.



Every session emits one `SESSION v=3 …` line plus a `SESSION_DETAIL` block (schema 3, `SessionSummary.SCHEMA_VERSION`). That is the point: a drive
capture in `evidence/` can run into gigabytes, and reading it is not a diagnostic strategy — diffing one
line per session is.

**Schema v=2 (2026-09-10) — and "every session" only became true with it.** Before v=2 the line was
emitted ONLY for a session that began at a USB attach, because `SessionSummary.begin()` was called
only from `UsbAttachActivity`. A launcher tap or an `am start` produced **no summary at all** — the
whole 2026-09-09 capture, a complete and successful session, contains zero `SESSION v=` lines. v=2
adds a launch origin, so:

- `origin=` is appended at the END of the line (`usb_attach` | `launch`). The field set and order are
  a versioned contract — grow it by bumping the version and appending, never by reordering, because
  these lines are diffed across weeks of captures.
- `perm_trampoline=none` and `serial=unknown` on a launch-origin session. These are **deliberately
  not fabricated**: there was no trampoline to sample, and `serial=sec_exception` is *defined* as
  "the attach-time grant did not land", so a launch-time read would mislabel itself. `grep
  perm_trampoline=false` therefore still matches only sessions where a trampoline really did see a
  missing grant, which is the fault-1 query.
- A launch-origin session superseded by a real attach closes as `exit=launch_superseded_by_attach`,
  distinct from `superseded_by_new_attach`.
- **v=3 (2026-10-08, D6):** `origin=` also reads `auto_attach` | `auto_boot` | `auto_bind` for a session whose
  link attempt the auto-claim started with no UI (adapter-attach broadcast, BOOT_COMPLETED, media-browser
  bind); the same absent-token rules as `launch` apply (`SessionSummary.kt:60-62`). No key moved or was added.

| Question | Grep |
|---|---|
| Did the USB permission grant land (dialog or cached "always")? | `grep 'USB HANDLER' … held=` and `grep 'prompted=' *.log` — `held=`/`prompted=` are legacy field names from the squat-era diagnostics (`UsbAttachActivity.kt`), kept on purpose post-revert since they still answer the same question about the ordinary attach flow. **Corrected 2026-10-08:** those lines come from `UsbAttachActivity`, which is dormant on this unit (manifest comment), so on an auto-claimed session the answer is the `auto-claim (<origin>): <DECISION>` line instead — `SKIP_NO_GRANT` = no grant; `CLAIM` followed by `claimed interface` = granted |
| Where does Bluetooth stall? | `grep -o 'btp_max=[A-Z_]*'` — `none`/`LINK_UP` = box never got RFCOMM; `AUTHENTICATING` = MFi; `IDENTIFYING` = iAP2 identification; `WIFI_HANDOFF` = BT succeeded, look at Wi-Fi |
| Bond asymmetry (the "forget it in iOS and it works" shape) | `grep 'bond_changed=true'` |
| Sessions that never produced video | `grep 'no_video=true'` |
| Did the box send something we still don't parse? | `grep 'CTRL unknown type'` |

`desc_fp=` is the adapter's descriptor fingerprint. A changed fingerprint across attaches means the USB
permission cache saw a different device — the leading hypothesis for the intermittent dialog;
`/script/ncm_only` flips the box between descriptor variants.

**Platform log flood (F-C5, recorded 2026-10-08, read-only bench).** `ethctrlmgr` writes ~1.2k lines/s into the
1 MiB main ring, so the ring holds seconds, not minutes, and the app's own logcat child costs ~3.4 % of a core
keeping up. Platform behaviour, not the app's; raising `persist.logd.size` is a device-owner action. The box's
`wireless` daemon's 1 Hz `EADDRINUSE` retry loop (SDP/HSP/HFP; F-C1, static box code) is 32-37 % of the app's
own lines on the same bench — see `11_HARDENING_PLAN.md` for both.

### 8.4 Not yet verified on the unit

Each item below is probed at start and the result goes into every file header, but none has been
confirmed on hardware:

- whether an `untrusted_app` may `exec /system/bin/logcat` at all under this unit's SELinux policy;
- which buffers open. `main,system,crash` should; `events`/`radio` are probed; `kernel` is opt-in and
  probably refused (`tools/drive_capture.sh` only gets `-b all` because it runs as the adb shell user);
- whether DocumentsUI exists here:
  `adb shell cmd package resolve-activity -a android.intent.action.CREATE_DOCUMENT -t text/plain`

Raise the ring buffer once at the bench so the backward drain reaches far enough — use
`persist.logd.size` rather than a runtime `logcat -G`, which does not survive a reboot.
