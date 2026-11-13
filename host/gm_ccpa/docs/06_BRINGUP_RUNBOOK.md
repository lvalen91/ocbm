# 06 — Bring-up runbook

What is proven on hardware, and the commands to repeat it. Context: [`04_SYSTEM_MODEL.md`](04_SYSTEM_MODEL.md)
→ [`05_SESSION_FLOW.md`](05_SESSION_FLOW.md) → this file. Items not measured on the unit are marked UNVERIFIED
or NOT bench-verified. Paths: `tools/test.sh`, `tools/build_apk.sh` are relative to `host/gm_ccpa`;
`tools/ocbmcmd.py`, `tools/ocbm_push.sh`, `tools/ocbm_install.sh`, `tools/uart_push.sh` are relative to the
`ocbm` root.

---

## 1. What is proven

Reproduced on the real truck (`gminfo37`, serial `<unit serial>`), not only the emulator. The keystone: `CH_MFI`
works from an ordinary app UID, the most load-bearing unknown in the architecture.

| Step | Evidence |
|---|---|
| Ordinary app claims `0x1314:0x2d00`; OCBM framing byte-compatible | `claimed interface 0 (class 0xff)`; `CT_HELLO_ACK v1 caps=0x0000003f` |
| `CT_SETTIME` applied | ack `status=0 (applied)` |
| MFi certificate over `CH_MFI` | 945 bytes, `30 82 03 ad 06 09 2a 86 48 86 f7 0d 01 07 02` (PKCS#7 signedData) |
| MFi signature over `CH_MFI` | 128 bytes (RSA-1024) |
| `CH_MGMT` identity snapshot | `<box BT name A>`, serial `<box serial>` |
| `CT_SUBSCRIBE` wakes the box radios | `host_present=1`, `hci0 UP RUNNING PSCAN ISCAN` |
| Box raises no SoftAP (`wifi_ap: false`); its own A/V layer still spawns (`AV_SUPPRESS=''`, `04` §2) but is unreachable | no `hostapd`, no `wlan0`, moal/mlan not loaded (NXP unit; the Realtek unit loads its WLAN driver before BT attaches, so `wlan0` exists, `04` §2) |
| iPhone completes the full BT handshake | §2 |
| App hotspot creds reach the `0x5703` source; it hands the iPhone the VEHICLE hotspot | `/etc/hostapd.conf` regenerated from the OCBM config; `replying 0x5703 (ssid="<vehicle SSID>" sec=Wpa2OrWpa3Personal)` |
| iPhone joins `br0`, advertises `_carplay-ctrl._tcp` | `num_connected_clients=1`, `<IPHONE_IP>` / `<MAC-1>`; `<phone name>._carplay-ctrl._tcp.local` |
| App reaches the phone by unicast | ICMP true, TCP 62078 + 49152 OPEN |
| App is the AirPlay endpoint, phone dials in | `POST /pair-setup` inbound, §6.1 |
| SRP-6a pair-setup verified against a real iPhone | M4/M5 reached, §6.2 |
| Rust core cross-compiles and runs on-unit | §6.3 |
| Full A/V session (HEVC + AAC, both stream SETUPs) | §7.1 |
| Untethered fault capture (no laptop attached) | §8 |

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

iOS retries `0x5702` (twice per session observed), so the handler must be idempotent. The accessory-initiated
reconnect path also completes.

## 3. Box-side changes (all reversible)

Two files on the adapter; nothing else was modified. Both default to stock when the keys are absent.

| File | Change | Restore |
|---|---|---|
| `/script/session_supervisor.sh` | `wifi_ap:` gate, `apply_host_wifi_creds()`, `AV_SUPPRESS` | `git checkout tools/session_supervisor.sh` in `ocbm`, then `tools/uart_push.sh` |
| `/etc/hostapd.conf` | ssid / passphrase regenerated from the app's OCBM config (channel only if supplied; the app supplies none) | `cp /etc/hostapd.conf.stock /etc/hostapd.conf` (backup made automatically) |

`wifi_handoff::read_hostapd_ap_config()` reads `/etc/hostapd.conf` unconditionally, so writing that file
changes what `0x5703` sends with no Rust rebuild; safe only because `wifi_ap: false` means no `hostapd` runs
from it.

---

## 4. Commands

### Build + install

```bash
bash tools/test.sh                 # Tier-0: tree-artifact guard, cargo tests, JNI cross-build, JVM tests (tools/jvm_tests.txt), docs budget, proto check
bash tools/build_apk.sh            # release flavor;  GMCCPA_BENCH=1 bash tools/build_apk.sh  = bench flavor
# -> apk/gmccpa-debug-<STAMP>[-bench].apk. The build prints the full path AND the install line; copy it.
```

**Build output never lives in the tree** (owner rule, `ocbm/CLAUDE.md`). Both scripts send every cargo call to
`CARGO_TARGET_DIR` under `$OCBM_CACHE/cargo-targets/`, use a `mktemp -d` APK work dir, expect `apk/` to be a symlink
into `$OCBM_CACHE` (created by hand; `build_apk.sh` only runs `mkdir -p apk`, which on a fresh clone is a real in-tree directory), and set `PYTHONDONTWRITEBYTECODE=1`. The gate's first step, `tools/tree_artifact_guard.sh`,
FAILS on any real `target`/`build`/`.gradle`/`__pycache__`/`.build`/`node_modules` directory under
`host/gm_ccpa` or `crates/` (symlinks pass); `GMCCPA_ALLOW_TREE_ARTIFACTS=1` runs the rest while cleaning up.

**Version stamp** (`build_apk.sh` header): `versionCode` = commit count (`git rev-list --count HEAD`);
`versionName` = `4.0+<STAMP>` (+`-bench`); `STAMP` = `<sha>` clean or `<sha>-dirty.<h8>` dirty, `h8` = first 8
hex of a sha256 over `git diff HEAD --binary` plus every untracked, non-ignored file (a git failure is FATAL,
never read as clean). The STAMP is the APK filename and `assets/build_sha` (printed by `IDENTITY`); there is NO
"latest" symlink, so the STAMP is the only tie between a binary and its sources. Dirty builds on one commit
share a `versionCode`; identity rides on `versionName`/`build_sha`. An artifact with a higher `versionCode`
cannot be installed over a newer build with `-r`: roll back with `adb uninstall -k android.car.usb.handler`
first (`-k` keeps the data, so the pairing survives).

**Install (owner-set).** Confirm `adb shell am get-current-user` prints `10`; stop and report if not.

```bash
adb install --user 10 -g -r -i com.android.vending apk/gmccpa-debug-<STAMP>.apk
```

- `-i com.android.vending` is the installer the in-motion path requires; eligibility is untested once vending is uninstalled (`01` §8). `-g` grants runtime permissions and
  `SYSTEM_ALERT_WINDOW` (`development` level).
- `--user 10` is mandatory: without it adb installs for every user. A user-0 install is not the driver's user and never claims
  (`AutoClaim.kt` step 1).
- **Any uninstall followed by a reinstall needs a 30 s wait** (owner rule). A plain uninstall also wipes
  `carplay_peers.bin`, the per-install accessory seed and the saved hotspot details (iPhone must re-pair, hotspot
  dialog returns); use `uninstall -k` only to keep them.

The package is `android.car.usb.handler`, source classes `zeno.gmccpa.*`; every `am`/`pm`/`appops`/`dumpsys`
command targets the package, and activities are `android.car.usb.handler/zeno.gmccpa.<Activity>` (the short
`/.MainActivity` form resolves against the package and fails). An old `zeno.gmccpa` install must be
uninstalled, or two copies coexist, each with its own `AdapterAttachReceiver` auto-claim. GM's fixed handler
starts the manifest alias `android.car.usb.handler.UsbHostManagementActivity` (target `UsbAttachActivity`) on
each attach, after granting this uid. Whether that makes the persistent grant below redundant is not
bench-verified: keep running `UsbGrant grant` after a reinstall.

### USB permission grant (re-run after every uninstall + reinstall)

A persistent grant, written once from the shell uid (holds `MANAGE_USB`) through an `app_process` helper,
removes the adapter's USB permission dialog (mechanism and limits: `docs/host/01_ANDROID_AND_AAOS.md`, "Bench
verification of the persistent USB grant"). The helper is not in `tools/` yet (session scratch, pending owner
approval to land it):

```bash
CLASSPATH=/data/local/tmp/usbgrant.dex app_process /system/bin UsbGrant <list|grant|revoke|status> [pkg] [userId]
# e.g. ... UsbGrant grant android.car.usb.handler 10     (adapter attached)
```

The entry is keyed on the adapter's descriptor strings and serial plus the app uid and user: re-run after any
uninstall/reinstall (new uid) and after a box swap. A plain `install -r` keeps it (bench 2026-10-08:
`is_granted=true`), though the same `-r` lost READ_LOGS (§8.1). `revoke` stores `is_granted=false`; run `grant`
to undo. Without it the auto-claim logs `SKIP_NO_GRANT` and never prompts; Start then asks through the app's
own claim loop (the ordinary attach resolver is never launched on this unit).

### Auto-claim with the app closed

Three triggers claim the adapter without the app open, each logged `auto-claim (<origin>): <DECISION> — <why>`:
`usb-attach` (`AdapterAttachReceiver`; hot plug or box reboot), `boot` (BOOT_COMPLETED), `media-browser bind`
(`CarPlayMediaBrowserService`, ~+7 s at boot when CCPA is the source). None raises a dialog; without the
persistent grant the decision is `SKIP_NO_GRANT`. While linked, one ongoing notification ("GM CCPA status"
channel) shows the state, with no names or addresses.

**`am force-stop` does NOT keep the app closed here** (bench 2026-10-08, `6c95684`): the system re-binds the
media browser within ~1 s and CarPlay is live again ~4 s later. Instead use the in-app **Stop** (suppresses
auto-claim until the next head-unit boot, `stop flag: boot identity = boot_count|boot_id|none`; Start or
Restart session clears it), or the `carplay_stop` + `ocbm_stop` verbs (process stays, nothing re-claims, but no
Stop-until-boot flag: the next launcher open starts it, ~4.9 s to CarPlay live).

**SYSTEM_ALERT_WINDOW rule** (admits the background FGS start and the CarPlay screen's background launch):
effective = appop `allow`, OR appop `default` AND permission granted. `install -g` grants it. Never set the
appop to `ignore`/`deny` (overrides the grant); `appops set --user 10 android.car.usb.handler
SYSTEM_ALERT_WINDOW allow` is optional and lost on uninstall. Preflight (read-only, no serials):

```bash
adb shell appops get --user 10 android.car.usb.handler SYSTEM_ALERT_WINDOW
adb shell dumpsys package android.car.usb.handler | grep -c 'android.permission.SYSTEM_ALERT_WINDOW: granted=true'
adb shell cmd package query-receivers --user 10 -a android.hardware.usb.action.USB_DEVICE_ATTACHED | grep -c android.car.usb.handler
adb logcat -d -b all | grep -a -E 'auto-claim \(|link hold|stop flag' | tail -20
```

`grep -a` is mandatory on these captures (binary bytes stop a plain grep). "Adapter reconnecting" with no
adapter is by design: a detach arms a relink bounded by `OcbmProbe.CLAIM_WAIT_MS` (10 minutes), then the link
hold is released (`SessionCore`, `releaseLinkIfUnlinked("relink: no link")`).

Headless bench checks (B-D6-0 … B-D6-5): replug with the app
closed, box reboot, cold boot, Stop for the boot, and missing credentials via the bench-only `finish_launcher`
verb. Pass = `auto-claim (<origin>): CLAIM` followed by `claimed interface` (§8.3).

### Steering-wheel Voice button → Siri (default assistant; NOT bench-verified)

Mechanism and design: `04_SYSTEM_MODEL.md` §4e. The wheel Voice key is captured by GM's `GMCarInputService`
(never a KeyEvent to an app); a short press no stock consumer takes opens a VoiceInteractionSession in the
**active assistant** (showFlags `0x23`); a long press never reaches it. If the iPhone also has Bluetooth HFP to
the radio, GM answers a ringing call itself and the press never reaches the app; what iOS does with a Siri tap
during a call is UNVERIFIED. With `android.app.role.ASSISTANT` held, `zeno.gmccpa.assist.AssistantSession`
(created by `AssistantSessionService`) sends one Siri tap (`requestSiri` buttondown 2, buttonup 3, as MacHost
`siriPress()`) and finishes. Stock Cinemo also sends a prewarm `RequestSiriWithLatency(1)` on key-down, varies
the sequence by `VRStateMachine` state (built-in VR off: 2,3; on: 3; ACCESSORY: 3; CONTROLLER: 2; INSPEECH:
nothing) and swallows voice presses during a CarPlay call; gm_ccpa always sends 2,3 (owner decision, iOS
decides). The role is not requestable in-app (roles.xml `requestable=false`); set it from the shell:

```bash
adb shell cmd role get-role-holders --user 10 android.app.role.ASSISTANT    # note it: the restore target
adb shell cmd role add-role-holder --user 10 android.app.role.ASSISTANT android.car.usb.handler
# Firmware normally writes both settings on role grant (observed); check them:
adb shell settings --user 10 get secure voice_interaction_service   # android.car.usb.handler/zeno.gmccpa.assist.AssistantService
adb shell settings --user 10 get secure assistant                   # same value
# If they did not follow, re-run add-role-holder (preferred). A settings-only fallback must set BOTH keys
# (GMCarInputService calls getAssistComponentForUser) and is fragile: the observer re-runs at user start.
adb shell settings --user 10 put secure voice_interaction_service android.car.usb.handler/zeno.gmccpa.assist.AssistantService
adb shell settings --user 10 put secure assistant android.car.usb.handler/zeno.gmccpa.assist.AssistantService
# Restore the previous assistant (the role is exclusive; this replaces Google Assistant for user 10):
adb shell cmd role add-role-holder --user 10 android.app.role.ASSISTANT <previous holder>
```

Holding the role also grants the `SYSTEM_ALERT_WINDOW` appop; restoring the previous holder resets it to
`default`, so after an install made without `-g` re-run the appop `allow` above. While held, system_server
keeps the assistant service bound, so the app process runs from boot. Re-run `add-role-holder` after an
uninstall + reinstall.

Revive logging first (§7). Watch: `adb logcat -s NETPROBE GMCarInputService | grep -a -E '\[vis  \]|Voice Assist|AssistUtils'`.
Expected on a short press:

```
[vis  ] ASSISTANT status (service): roleHeld=true voice_interaction_service=android.car.usb.handler/zeno.gmccpa.assist.AssistantService ...
[vis  ] service created / service ready (connected; assist structure + screenshot disabled)
GMCarInputService: Receive Voice Assist Key Short Press / voice key, invoke AssistUtils
[vis  ] session onShow showFlags=0x23 (WITH_ASSIST|WITH_SCREENSHOT|SOURCE_PUSH_TO_TALK) pushToTalk=true
[vis  ] session onHandleAssist (expected on every press: null assist data; nothing extra sent)
[vis  ] Siri tap (wheel/assist show) result=3: sent requestSiri buttondown(2)+buttonup(3)
[vis  ] session onHide
```

The `Siri tap` line comes from the `cp-event-tx` thread, so its position varies. `result=-1`/`-3` =
`DROPPED — CarPlay not connected` (normal with no session); a teardown between the `session_started` gate and
the send logs `FAILED`/`PARTIAL`. `ASSISTANT status (launcher)` prints once per process from MainActivity,
role held or not. Bench check: press Voice again while Siri is listening/answering and record what iOS does
(expected: toggles/ends Siri; intended, no Siri-active guard).

Other wheel keys (decompile only, no press log): the **hangup** key (1003 → `KEYCODE_VOLUME_MUTE` 164) never
reaches the app (GMCarInputService ends the call or offers it to projection/OnStar; the window manager routes
164 only to AudioService; expect `onKeyEvent(KeyEvent { … keyCode=164 … })` / `handle ENDCALL key` and nothing
from the app); CarPlay's answer/end are the uid-5 Telephony HID, not advertised in `/info` (§6.4). The **call**
key (1001 → `KEYCODE_CALL` 5) CAN reach a focused gm_ccpa activity (UNMEASURED): with no key handler,
`PhoneFallbackEventHandler` would start `ACTION_CALL_BUTTON` (short) or `ACTION_VOICE_COMMAND` (long). Bench:
CarPlay focused, no call ringing, `adb logcat -b all | grep -a -E 'GMCarInputService|CALL_BUTTON|VOICE_COMMAND|START u10'`
while pressing short, then long.

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

- No channel is passed (owner decision): the app sends no `wifi_channel`, so the box keeps its stock hostapd
  channel; `--es chan` is not read.
- With no saved credentials and no `--es ssid/pass`, a start or USB attach opens the credentials dialog (no
  compiled-in credentials). `--es ssid/pass` apply to that run only; only the dialog's Save writes `HotspotPrefs`.
- The passphrase cannot be read programmatically: read it from Settings ▸ Hotspot (`com.gm.hmi.connection`'s
  `WifiHotspotActivity`) and type it in, or pass `--es pass`.

Other `--es run` verbs (`MainActivity.handleRunExtra`): `full` (BT handoff + Wi-Fi endpoint, the real
end-to-end sequence, §7.1), `carplay_rx` (Wi-Fi endpoint only), `carplay_ui`, `mdns_self`, `carplay_stop`,
`ocbm_state`, `ocbm_disconnect`, `ocbm_forget`, `ocbm_stop`, `capture_status`, `capture_whole_os`,
`capture_own`, `export_log`, `export_log_raw` (§8.2). A verb executes once per delivery: a launcher recreate
(config change, process death restored from recents, task re-selected) re-delivers the intent but does NOT
replay it; it auto-starts, and the retained intent is stripped of `run`/`ssid`/`pass`/`mac` (`IntentReplay`).

**Bench-only verbs** (a release build answers `mic_inject: not in this build (release)` / only logs; build with
`GMCCPA_BENCH=1 tools/build_apk.sh`):
- `mic_inject` streams a pushed 16 kHz mono S16LE WAV into the open uplink gate in place of the vehicle mic
  (lead silence, clip, tail silence, then the real mic), paced by AudioRecord. Enable per test with
  `adb shell setprop debug.gmccpa.mic_inject 1` (`0` after; resets on reboot): MainActivity is exported, so
  while it is 1 any app could send the verb. Args: `--es file <path>|-|none`, `--ei lead_ms` (700),
  `--ei tail_ms` (1500), `--ei expire_s` (60). Clip: `say -o p.wav --file-format=WAVE --data-format=LEI16@16000
  --channels=1 "<text>"`, `adb push` to `/data/local/tmp/gmccpa_mic.wav`, `chmod 644`; if unreadable,
  `--es file -` takes it over `toybox nc 127.0.0.1 9119` within 30 s. `--es file none` clears an armed clip
  even with the property 0 (`bench/BenchHooks.kt` KDoc).
- `finish_launcher` applies any `--es ssid/pass`, then finishes MainActivity to test the headless auto-claim
  (BACK only backgrounds this singleTask root). Pass: `launcher view detached — the session core runs on headless`.

### Watch the box

`uart_cmd.sh OUTFILE SECONDS 'command'` is the local, git-ignored `scratchpad/uart_cmd.sh` at the `ocbm` root;
the tracked `host/uart_cmd.sh` takes `"command" [seconds]` and prints to stdout (do not mix them).

```bash
cd ../..   # host/gm_ccpa -> ocbm root
bash scratchpad/uart_cmd.sh /tmp/u1.txt 8 'tail -40 /tmp/wl.log'   # BT / iAP2 / handoff trace
tr -d '\r' < /tmp/u1.txt | tail -40
```

Send one or two short commands per call (long lines come back truncated or as `/bin/sh: ???…: not found`) and
strip `\r`. A bare prompt usually means the command was too long, not that the box is wedged.

**Prefer OCBM over the UART whenever the adapter is on USB;** the UART is for recovery (box will not
enumerate, or a flash). The app follows the box's `CH_LOG` stream and prints each line as `[box:<source>]`
(box, airplayd, iap2d, aa-bridge, bt, wl, `radio_*`; `ocbmd` and the supervisor narrate into `/tmp/box.log`).
`wlan.log`, `wlan_off.log`, `supervisor.log` and `ocbm_boot.log` are only snapshotted at session end
(`OcbmProbe.BOX_LOG_SNAPSHOT_FILES`).

| Want | Use |
|---|---|
| Run a command on the box | `python3 tools/ocbmcmd.py "<cmd>"` (one-shot, `CH_CONSOLE`) |
| Interactive root shell | `ocbm-host console` |
| Read a box file | `ocbm-host pull 1314 2d00 <remote> <local>` (`CH_FILE`, CRC-checked) |
| Deploy a binary or script | `tools/ocbm_push.sh <local> <remote> 755` |
| Watch box logs live | nothing: read the `[box:<source>]` lines in logcat |

`ocbmd` gives `CH_CONSOLE` its own queue drained after audio and video, so a chatty `tail -f` cannot starve a
live session.

**`tools/ocbm_push.sh` is not an install.** Its default set is `ocbmd` + `btd`; pushing `session_supervisor.sh`
without `/script/radio_hal.sh` and `radio_detect.sh` leaves no radio bring-up and no error anywhere. That is how
Bluetooth died silently once (`docs/ops/06_CORRECTIONS_LEDGER.md` R-20W-5): `hciattach` failed with `Can't set
line discipline: Invalid argument` beneath a flawless firmware download, and the supervisor never reads the
exit status of the detached script. Use `ocbm_install.sh --full` for a real deploy; after pushing the
supervisor verify `ls /script/radio_hal.sh /script/radio_detect.sh`.

---

## 5. Gotchas

1. **Do not cycle `host_present`.** Repeated app stop/start inside ~20 s trips the supervisor's flap detector
   (escalates to an `ocbmd` restart, then a full box reboot). Connect once; the 1 Hz heartbeat holds the link.
2. **The bulk endpoints are IN `0x81` / OUT `0x01`**, not `0x83`/`0x02`. Discover the pair by walking the
   interface; hardcoded addresses fail.
3. **A bonded phone is paged by the box; the phone need not initiate.** Measured on the mainline baseline
   (revision 8, iOS 27.2): every box-initiated reconnect with the phone in range succeeded,
   `[reconnect] … driving reconnect` → `[sdp-client] L2CAP SDP channel up (phone paged)` (~1 s) →
   `RFCOMM connected OUT … ch 1` → `IdentifyAccept` → 0x5703 in ~3 s, including the hands-off cold boot after a
   radio reboot. Out of range: `CONNECT_FAILED status=0x04` (page timeout, ~5 s), backoff 10 → 60 s. Settings ▸
   Bluetooth on the iPhone is needed only for the FIRST pairing or after a lost bond.
4. **A stale hotspot poisons the next session.** After handing out credentials for a network that no longer
   exists, forget that network on the iPhone before retrying.
5. **`pkill -f session_supervisor` kills the shell running it** (its cmdline matches). Use an explicit PID or
   the `[s]ession_supervisor` char-class trick the script uses.
6. **MFi can return status 1 transiently** while `btd` is mid-bring-up doing its own chip calls: observed once
   as an immediate (~2 ms) `copy_certificate` failure with the signature succeeding moments later; every later
   attempt returned the full 945 bytes. Worth a soak loop before trusting it in the session path.
7. **A box reboot leaves `ocbmd` exiting for respawn** until a host reconfigures the gadget. If `HELLO` times
   out, check `/tmp/ocbmd.log` for `accessory POLLHUP/POLLERR` and that
   `/sys/class/android_usb_accessory/android0/state` reads `CONFIGURED`.
8. **logcat boots effectively dead on this unit.** Revive it before concluding anything from silence (§7).

---

## 6. The Wi-Fi endpoint: proven, plus the byte-level details that made it work

Ported from carplayd's mDNS advertiser and connect-out (`box/carplayd/src/discovery.rs`), not inferred.
Verified on the truck end to end: advert with the Car bit, TXT set matching carplayd's, SRV resolving to a
reachable br0 address, listener bound, browse + resolve of the phone's `_carplay-ctrl._tcp`,
`GET /ctrl-int/1/connect` accepted, phone dialing the control connection back in.

**The fix that made the connect-out work:** `AirPlay-Receiver-Device-ID` is a decimal uint64, not a MAC string
(carplayd's `mac_to_dec()` folds the MAC). Sending `<MAC-3>` makes iOS parse it base-10 as `0`, find no
"receiver 0" among the browsed peers, and have nothing to dial back to. TXT `deviceid` and `/info` `deviceID`
keep the colon MAC; the asymmetry is deliberate.

Other reference details: `Host:` for an IPv6 peer is bracketed with the zone id stripped (the zone belongs to
the socket); CarPlay version family is **320.17**, not AirPlay-2 980.x; no `CSeq` on the connect-out (plain
HTTP, not RTSP); retry the nudge 10×1 s and clear dial memory on `ServiceRemoved`; never answer an
unimplemented route with a bodyless 200 (a false success on pair-verify M1 is worse than an error); echo the
request's protocol version.

**Two platform facts that are not negotiable:**

1. **GM's own CarPlay receiver cannot be disabled from shell.** `pm disable-user` on
   `com.gm.domain.server.delayed/...CarPlayService` fails with `SecurityException: Shell cannot change
   component state` (a PERSISTENT guard); `am start-service` cannot restart it; it returns on reboot. `:7000`
   stays held and `CarPlay._airplay._tcp` keeps advertising from `192.168.5.1`: coexist with a distinct
   Bonjour instance name and port. `pm uninstall --user 10` does work, if ever the right tool.
2. **`NsdManager` cannot pin the advertised address or SRV host** the way carplayd does with `RX_ADDR`. The
   app therefore runs a self-hosted `MdnsResponder` (owns `gmccpa-rx.local`, publishes `br0` only) alongside
   `NsdManager`; design in `04_SYSTEM_MODEL.md` Phase 4.

### 6.1 Milestone (2026-08-04): the iPhone opens the CarPlay control connection to the app

The full chain closes on the truck into an ordinary non-privileged app (BT handshake → MFi cert+sign →
`0x5703` handoff to the vehicle SoftAP → phone joins `br0` → our advert indexed by iOS → connect command
matched → the phone dials our app), every step device-proven:

```
[WiFi] Bonjour device added/updated
carManager_handlePendingAutoconnect: index = WiFi, for deviceID: <MAC-4>,
    pendingAutoconnectID: <MAC-4>
>>> INBOUND CONTROL CONNECTION from fe80::<phone-derived>%br0:58503
--- POST /pair-setup
```

Three things made it work:
1. **`NsdManager` for the advert.** The only thing observed to produce `[WiFi] Bonjour device added/updated`
   (entry in iOS's Wi-Fi endpoint index). A hand-written responder with byte-perfect records (checked against
   dnslib, scapy, `mdns-sd`) never produced an endpoint.
2. **Advertise before dialing.** `GET /ctrl-int/1/connect` sets a pending autoconnect resolved immediately
   against the already-populated index; dialing in the same breath as advertising loses the race by ms.
3. **Letting the cached endpoint age out** rather than forcing a fresh one mid-session.

**Pin the identity before a run, never vary it inside one.** iOS indexes the endpoint by `deviceid` for the
record TTL; changing it mid-session guarantees a spurious `No matching endpoint found` (a diagnostic identity
swap produced exactly this and looked like a protocol failure):

```
carManager_handlePendingAutoconnect: pendingAutoconnectID: <MAC-3> is expired (55.682 seconds old)
```

GM's `CarplayService` was force-stopped during this run (`:7000` free). Coexistence with GM's service running
is the shipping configuration (status: `12_OBSERVED_FLOW.md` Failure Point 3).

### 6.2 SRP-6a pair-setup verified against a real iPhone

The shipping receiver does pair-setup in the Rust core (`NativeCore.kt`); with no core the connection is
closed. This capture, from a Kotlin port of `pairing/src/srp.rs` + `tlv.rs`, proves SRP wire-compatibility with an
iPhone:

```
--- POST /pair-setup
pair-setup M1 -> M2 (salt 16B, B 384B, setup code 3939)
pair-setup M3: A=384B proof=64B — verifying SRP
*** MILESTONE: SRP-6a PROOF VERIFIED against a real iPhone — M4 going out
*** pair-setup M5 reached — the Ed25519 LTPK exchange
```

The phone advancing to M5 means our proof matched its own computation. This closes the gap `pairing/src/setup.rs`
flags as unprovable offline, including the HomeKit divergence that `H(g)` is taken over the minimal generator
byte `[5]`, not padded to the modulus length. SRP-6a needs no third-party crypto (`BigInteger` + SHA-512 exist
at API 32); M5/M6 (Ed25519 LTPK) and pair-verify (X25519 + Ed25519) do not, hence the Rust core.

### 6.3 Build toolchain for the Rust core

- NDK `30.0.15729638` in Android Studio's SDK, `x86_64-linux-android32-clang` (the head unit's exact ABI/API).
- Rust targets `x86_64-linux-android` (head unit) + `aarch64-linux-android` (emulator) via rustup.
- `cargo-ndk` 4.1.2 panics on this workspace; not needed.
- `/opt/homebrew/bin/cargo` shadows rustup's shim and cannot add cross-targets: use `$CARGO_HOME/bin/cargo`.

```bash
export PATH="$CARGO_HOME/bin:$PATH"
NDK=$ANDROID_HOME/ndk/30.0.15729638/toolchains/llvm/prebuilt/darwin-x86_64/bin
export CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER="$NDK/x86_64-linux-android32-clang"
cargo build --release --target x86_64-linux-android
```

Result: the crates cross-compile clean and a `cdylib` linking `receiver` + `pairing` loads on the head unit
(`ELF shared object, 64-bit LSB x86-64, for Android 32, built by NDK r30-beta2`). `x25519-dalek` and
`ed25519-dalek` cross-compile, so no third-party Java crypto provider is needed.

**The `eld-codec` trap:** a workspace-root build pulls it in via feature unification and fails on a missing
`fdk-aac` header. Build `receiver` from its own directory with `--no-default-features --features mic-uplink`
(the default also enables `local-mfi`); this is the recipe `tools/test.sh` uses for Tier-0.

### 6.4 `/info`: generated from the reference, every number measured on the unit

`/info` derives from `receiver::info::build_info()` (`crates/vendor/receiver/src/info.rs`); the shipped
`assets/info.bplist` is adjusted by `tools/info_plist_*.py` and served as `application/x-apple-binary-plist`. Not hand-written: iOS validates this
capability set at RECORD and tears the session down if incomplete. It holds every key the reference's
regression test lists as validated at activation, plus `modes` (without it the screen resource is `type=N/A`
→ `StartupFailed -17483`).

| Property | Measured on `gminfo37` | In `/info` |
|---|---|---|
| Panel | `2400x960`, `DD134IA-01B` | `widthPixels 2400`, `heightPixels 960` |
| Refresh | 60.001434 Hz | `maxFPS 60` |
| Density | 200, `192.911 x 193.523` dpi | `widthPhysical/heightPhysical 0` (matches the genuine CCPA) |
| App window | `2400x960`, full panel | — |
| System bar insets, normal | `left=189 top=118`; GM chrome overlays the panel | — |
| `mAppBounds`, normal | `Rect(189, 0 - 1605, 960)`; usable view area `1416x842` = bounds minus the 118 px overlay. Left 189 is the LeftBar; the right 795 is a navigation-class inset provider (GM's widget pane), NOT a bar. The 118 px TopCarSystemBar is status-class: it overlays and does NOT reduce `mAppBounds` (top stays 0; the 842 height subtracts it separately) | second `viewAreas[]` entry |
| System bar insets, immersive | `0,0,0,0` | no `safeArea`, `viewAreas` lever off |

The immersive flag is a hard requirement **for the full-bleed view area only**: when CarPlay owns the panel,
without `IMMERSIVE_STICKY | FULLSCREEN | HIDE_NAVIGATION` the top-left `189x118` is covered by GM's chrome
and CarPlay would put interactive UI under it. In the SECOND view area it inverts: GM's chrome stays VISIBLE
and immersive is dropped.

Insets act on the CONTENT, never the window (`mBounds` stays `2400x960`): only the three hide flags and
`FLAG_FULLSCREEN` may toggle; clearing the `LAYOUT_*` flags displaces the surface to `377,236`. Rules and
reasoning: `04_SYSTEM_MODEL.md` §4d.

**Audio matches the hardware:** every output thread is 48000 Hz, `AUDIO_FORMAT_PCM_16_BIT`, stereo,
`channels=0x2`, no multichannel (CarPlay's stereo-48k ceiling costs nothing).
- type 100 `default`/`telephony`/`speechRecognition`: AAC-ELD 16k mono in and out (mic uplink)
- type 100 `alert`, type 101 `default`: AAC-ELD 48k stereo out
- type 102 `media`: AAC-LC 48k stereo, the high-latency path so music avoids the voice channel
- type 100/101 `compatibility`: PCM fallbacks

**Input: touch + media buttons only.** `hidDevices` carries `CarLink Touchscreen` and `CarLink Media Buttons`
(both with `displayUUID` matching the display `uuid`, which iOS checks). D-pad, knob and telephony-button
levers are off, so `displays[].features` is `0x0A`, not `0x1A`. Wheel media buttons ride this HID path and do
not depend on the metadata plane. If identity or geometry changes, patch/regenerate via `tools/info_plist_*.py`;
`deviceid` / `pi` / `features` must stay byte-equal to the Bonjour TXT or pair-verify fails.

---

## 7. A/V capture, the logcat trap, and a full session

**The head unit boots with logging effectively dead:** all four `logcat` ring buffers read 0 bytes, each 64 KiB.
Nothing from the app appears, which looks exactly like the app failing to run; silence is not evidence. Fix
it at the start of every session:

```bash
adb shell "setprop persist.log.tag V"   # revives logd; survives reboot
adb logcat -G 16M                       # 64 KiB is ~2 s of A/V logging
```

**Verifying a session without logs (the socket table).** Derive the uid; it changes on every
uninstall/reinstall, and a stale literal shows an empty table on a healthy session.

```bash
APPID=$(adb shell dumpsys package android.car.usb.handler | sed -n 's/.*userId=\([0-9]*\).*/\1/p' | head -1)
SOCKUID=$((1000000 + APPID))     # user 10 => 10*100000 + appId; on user 0 it is just $APPID
adb shell "cat /proc/net/tcp6" | awk -v u="$SOCKUID" '$8==u && $4=="01"'   # 01 = ESTABLISHED
```

4–5 established sockets to the iPhone's `fe80::…%<if>` plus `POST /feedback` every 2 s = healthy session. GM's
own receiver owns `:7000` (`0x1B58`) under uid `1001000` and listens again, so coexistence is the live
condition. Do not leave `nc -l -p 9001` running from manual probing: it steals the seam port, and the
receiver's forwarder then connects to `nc` instead of the CarPlay screen's decoders.

### 7.1 Bringing up a full A/V session

The sequence that produced the working session in `evidence/session_2026-08-05/WORKING_SESSION.md` (the
`evidence/` archive is in the archived standalone `gm_ccpa` checkout, not this tree; `00_HANDOFF.md` §8).
Assumes the CCPA adapter is on USB and the iPhone is paired to the vehicle over Bluetooth.

**Step 0. Revive logging first** (§7). Nothing below is observable without it.

**Step 1. Start the session (BT handoff + Wi-Fi receiver).**

```bash
adb shell am force-stop android.car.usb.handler
adb logcat -c
adb shell "am start -n android.car.usb.handler/zeno.gmccpa.MainActivity --es run full \
           --es ssid '<vehicle SSID>' --es pass '<passphrase>'"
```

The CarPlay screen opens by itself when the phone opens the control connection (`CarPlayRx.onSessionUp` →
`SessionCore.showCarPlayScreen`); no further command is needed. Wait for both stream SETUPs:

```bash
adb logcat -d -s NETPROBE | grep -E "SETUP phase2"
#   SETUP phase2 screen(110) ... -> dataPort NNNNN
#   SETUP phase2 audio(102) ... AacLc audioType="media" -> dataPort NNNNN
```

**Step 2. Expect the CarPlay screen to come up on its own** (it starts at session-up, before the stream SETUPs):

```
[cpui] session up: seams listening on :9001 (HEVC), :9002 (AAC-LC), :9003 (voice, routed), :9004 (metadata); audio is surface-independent
[cpui] renderer attached to Surface
[hevc] MediaCodec configured: video/hevc 2400x960 ... decoder=OMX.Intel.hw_vd.h265
[hevc] keyframe — decoding starts
[hevc] FIRST FRAME RENDERED
[aac ] FIRST AUDIO FRAME PLAYED
```

**Step 3. Watch it:** `adb logcat -d -s NETPROBE | grep -E "\[hevc \]|\[aac  \]|\[cpui \]"`. `MainActivity`
commands are safe during a live session: they detach the video renderer while audio and the seams keep
running; `carplay_ui` (or a launcher resume) reattaches it. Health without logs: the socket table in §7.

**Step 4. Restore the screen after anything backgrounds it:** `--es run carplay_ui` (refused with `carplay_ui refused: no live CarPlay session` when no session is live). Video returns in ~250 ms
via the producer's re-dial + ForceKeyFrame; audio is never interrupted.

**Do NOT** `am start -n android.car.usb.handler/zeno.gmccpa.av.CarPlayActivity`: it is `exported="false"` and
`adb shell` (uid 2000) lacks `START_ANY_ACTIVITY` on this user build (`SecurityException: … not exported from
uid 1010123`). Always go through `MainActivity`.

#### Troubleshooting

| Symptom | Cause | Action |
|---|---|---|
| App dials, phone answers `HTTP/1.1 200 OK`, no inbound on `:7011` | Stale iOS endpoint cache; iOS logs `carManager_handlePendingAutoconnect: No matching endpoint found for deviceID …` then `-6753 kConnectionErr` | Forget the vehicle on the iPhone (Settings → General → CarPlay) and re-pair over Bluetooth. Confirmed fix; not caused by GM's `:7000` |
| Black screen, session otherwise healthy | video seam bound but no frames | check `[hevc]` lines; `first frame decoded (0 B Annex-B)` means the sample-description (AVCC→Annex-B) unwrap regressed (`12_OBSERVED_FLOW.md`, `forward.rs`) |
| `bind :9001 … EADDRINUSE` | a stale CarPlayActivity generation or a manual `nc` holds the seam | the seam bind retries across TIME_WAIT on its own; kill any manual `nc -l -p 9001` |
| No `[hevc]`/`[aac ]` lines and no app logs | logcat is dead | `setprop persist.log.tag V`, `logcat -G 16M` (§7) |
| Nothing after a reboot | GM's `CarplayService` self-restarts and re-claims `:7000` | verify coexistence live each time (§6) |

### 7.2 The credential-ordering trap and the stale-resolve trap

Neither errors at the point of the mistake; both surface later as "CarPlay just doesn't start."

#### Trap 1: a credential-less `CT_SUBSCRIBE` pins `0x5703` to the box's stock SSID for the whole session

The box applies host Wi-Fi credentials only inside `wireless_up()` (`tools/session_supervisor.sh`:
`apply_host_wifi_creds()` is called from it), which runs on the `host_present` 0→1 edge that `CT_SUBSCRIBE`
triggers. Subscribe once without credentials and the stack comes up on the stock `/etc/hostapd.conf`; a later
`CT_SUBSCRIBE` with credentials does not re-apply them. Observed: the app sent a 153-byte config with
`wifi_ssid: <vehicle SSID>`, the box logged nothing wrong, and `/etc/hostapd.conf` still read
`ssid=<box stock SSID>`: `0x5703` would hand the iPhone an AP that is never raised (`wifi_ap:false`) and, per
§5 item 4, poison the next attempt.

**Guard:** all credential entry points (button, USB attach, `--es run`) go through
`SessionCore.applyHotspotFields()`, and `OcbmProbe` refuses the radio-wake edge with no SSID
(`CT_SUBSCRIBE refused: no hotspot …`).

**Recovery:** release the box for at least 20 s (flap detector), then relaunch with `--es ssid/--es pass`.
`am force-stop` cannot provide the gap (media-browser bind re-claims within seconds); use the in-app Stop or
`ocbm_stop`. Verify:

```bash
bash scratchpad/uart_cmd.sh /tmp/h.txt 11 'cat /etc/hostapd.conf'   # ocbm root, local scratchpad script
tr -d '\r' < /tmp/h.txt | grep -aE '^ssid=|^channel=|^wpa_passphrase='
```

Expect `ssid=<vehicle SSID>`; `ssid=<box stock SSID>` means the credentials did not reach `wireless_up`.

#### Trap 2: `NsdManager` serves a stale peer address and port, and restarting discovery does not fix it

Symptom: handoff succeeds, the phone joins `br0`, `_carplay-ctrl._tcp` resolves, and every
`GET /ctrl-int/1/connect` times out after 3000 ms; iOS logs `carManager_handlePendingAutoconnect: no pending
autoconnections` (the connect-out never set it, §6.1). The neighbour table shows the dial target is a ghost
(iOS rotates its IPv6 privacy address and control port; cached port `57236` vs live SRV `57292`):

```
$ adb shell ip neigh show dev br0
<IPHONE_IP>         lladdr <MAC-5> REACHABLE   <- the phone, actually here
<IPHONE_LL6>        lladdr <MAC-5> STALE       <- and here
<IPHONE_GHOST_LL6>  INCOMPLETE                 <- what we were dialling
adb shell "ping6 -c 2 -W 2 <resolved-addr>%br0"    # "Address unreachable" = confirmed
```

`INCOMPLETE` against the address in the `resolved '<peer>' -> …` log line confirms it. Fix (in code): the wire
is the authority; `MdnsInspect.liveEndpoint()` queries SRV then A/AAAA and `CarPlayRx.dialCandidates()` dials
those first (re-querying at attempts 3 and 6), the cached pair last. Mechanism: `12_OBSERVED_FLOW.md`
"FAILURE POINT 2".

---

## 8. Untethered capture

The two faults that survive every tethered session (the USB permission dialog on roughly half of adapter
attaches, absent with the persistent grant, §4; and the iPhone refusing to auto-reconnect until iOS forgets
the box) only happen with no Mac attached, so the app captures itself. `logcat -d` dumps the ring buffer, so
capture need not be running at the fault, only within the ring window afterwards. Capture is armed
idempotently from every entry point (`MainActivity.onCreate`, `AutoClaim.trigger`, `BootReceiver`,
`CarPlayMediaBrowserService.onCreate`, `UsbAttachActivity` when it runs; `LogCapture.start` call sites) and
drains the backlog before tailing. `UsbAttachActivity` is reached through the `UsbHostManagementActivity`
alias after GM's handler grants the uid; before the grant, `AdapterAttachReceiver` is the live route.

### 8.1 One-time bench grant

Default scope is whole-OS (`CapturePrefs`); without `READ_LOGS` it degrades to own-process (always works) and
says so in the capture header. `READ_LOGS` is `signature|privileged|development`: declaring it is not enough,
but adb can grant it (the app declares it so `pm grant` accepts it).

```bash
adb shell pm grant --user 10 android.car.usb.handler android.permission.READ_LOGS
adb shell am force-stop android.car.usb.handler       # NOT optional
```

The remediation the app prints (`LogCapture.grantCmdForUser`/`forceStopCmd`) is built from the running package
name, so it is safe to copy.

**The force-stop is the working remedy, cause unconfirmed.** A process that predates the grant can keep reporting
`GRANTED` while `logd` still filters it to its own uid; the usual explanation (the `log` gid is assigned at
fork) is refuted on this unit, where a process forked at boot also degraded (`11_HARDENING_PLAN.md` N25, root
cause OPEN; `LogCapture` prints a different remediation after a fresh-process failure). Before the first
capture file the engine asks logd once (`logcat -d -t 2000 -v threadtime -v uid`, retried up to 15 s only while
the probe cannot run or returns no line) for a line of another uid; none = degrade the reported scope and print
the force-stop command. Believe `effective`, never the granted flag. A probe that cannot run logs `whole-OS
capture UNVERIFIED` and capture continues as requested.

The grant survives reboots and is lost by a full uninstall. The 2026-10-08 bench also saw `adb install -r`
lose it (§4): check `capture_status` after any install and re-run the grant when scope reads degraded.

### 8.2 Operating it

```bash
adb shell am start -n android.car.usb.handler/zeno.gmccpa.MainActivity --es run capture_status   # scope, buffers, bytes, drops
adb shell am start -n android.car.usb.handler/zeno.gmccpa.MainActivity --es run capture_whole_os
adb shell am start -n android.car.usb.handler/zeno.gmccpa.MainActivity --es run capture_own
adb shell am start -n android.car.usb.handler/zeno.gmccpa.MainActivity --es run export_log
```

The launcher carries only Export (**Adapter & logs ▸ Export logs**); scope and status are these verbs only.
The same sheet holds Restart adapter Bluetooth, Reboot adapter, Forget pairing (each confirmed) and the full
"Last message" (also logged as `ui: <STATE> — <detail>`). `MGMT_GET_INFO` runs on every link-up; `ocbm_state`
reads the box state.

Export walks a ladder and reports the rung: SAF picker → mounted USB volume (`getExternalFilesDirs()`) →
`MediaStore.Downloads` → path in `filesDir`. This unit has DocumentsUI (bench 2026-10-08, `230eaa7`), but the
picker opens in its own task, which the CarPlay screen covers: the export sat pending until the app was brought
forward (cancel processed ~43 s later, then MediaStore Downloads). Read it with
`adb shell content read --user 10 --uri content://media/external_primary/downloads/<id>`; `adb pull` of the
Download path is permission-denied for the shell and `run-as` is blocked.

**Exports are redacted by default.** A whole-OS capture contains the hotspot passphrase, the iPhone's BR/EDR
MAC (in `[box:…]` lines and the `MGMT_INFO` snapshot; `CT_PHONE_IDENT.deviceID` stays EMPTY in this role, F-C8),
the adapter serial and `CarLink-xxxx` name, the VIN and GPS, and an export leaves the vehicle on a stick.
`export_log_raw` opts out and is a separate verb on purpose. Rules and known gaps: `logging/Redactor.kt` class
KDoc.

### 8.3 What to grep

Start from a plain `adb logcat > logcat.log`; the unfiltered file is what shows the fault was NOT the app.

```bash
grep NETPROBE logcat.log                                    # the app's own narrative
PID=$(grep -m1 'IDENTITY pid=' logcat.log | grep -o 'pid=[0-9]*' | cut -d= -f2)
awk -v p="$PID" '$3==p' logcat.log                          # + every framework line the app caused
grep -E 'EXPECTED-MISSING|EXPECTED-LATE' logcat.log         # steps that did not happen, or nearly
grep '## STATUS' logcat.log                                 # standing state, nearest block to any offset
grep ' E NETPROBE' logcat.log                               # faults
grep -E 'iPhone DETECTED|INBOUND CONTROL|MILESTONE' logcat.log
```

Both filters are needed. In the 2026-09-09 capture the app's PID emitted ~250 lines under other tags
(`CCodec` 68, `CCodecConfig` 55, `CCodecBuffers` 45, `BufferQueueProducer` 17, `MediaCodec` 11,
`ViewRootImpl[CarPlayActivity]` 8, `SurfaceUtils` 7), the lines that separate an app decode fault from a
platform one. `IDENTITY` anchors the PID filter and carries the build SHA (`assets/build_sha`; `pm install`
discards the SHA in the filename).

Every session emits one `SESSION v=3 …` line plus a `SESSION_DETAIL` block (`SessionSummary.SCHEMA_VERSION`);
diff one line per session instead of reading gigabytes. The field set and order are a versioned contract (bump
the version and append, never reorder). `origin=` is last: `usb_attach` | `launch` | `auto_attach` |
`auto_boot` | `auto_bind` (`auto_*` = started by the auto-claim with no UI). A session not begun at a USB attach
logs `perm_trampoline=none` and `serial=unknown` (deliberately not fabricated; `serial=sec_exception` is
*defined* as "the attach-time grant did not land"), so `grep perm_trampoline=false` matches only sessions where
a trampoline really saw a missing grant. A launch-origin session superseded by an attach closes as
`exit=launch_superseded_by_attach` (distinct from `superseded_by_new_attach`).

| Question | Grep |
|---|---|
| Did the USB permission grant land? | `grep 'USB ATTACH TRAMPOLINE' … held=`, `grep 'prompted=' *.log` (names kept from the earlier trampoline; same question). These come from `UsbAttachActivity`, which runs only when GM's handler starts the alias; on an auto-claimed session read `auto-claim (<origin>): <DECISION>`: `SKIP_NO_GRANT` = no grant; `CLAIM` then `claimed interface` = granted |
| Where does Bluetooth stall? | `grep -o 'btp_max=[A-Z_]*'`: `none`/`LINK_UP` = box never got RFCOMM; `AUTHENTICATING` = MFi; `IDENTIFYING` = iAP2 identification; `WIFI_HANDOFF` = BT succeeded, look at Wi-Fi |
| Bond asymmetry ("forget it in iOS and it works") | `grep 'bond_changed=true'` |
| Sessions with no video | `grep 'no_video=true'` |
| Box sent something we don't parse | `grep 'CTRL unknown type'` |

`desc_fp=` is the adapter's descriptor fingerprint; a change across attaches means the USB permission cache saw
a different device (leading hypothesis for the intermittent dialog; `/script/ncm_only` flips descriptor
variants).

**Platform log flood (F-C5, read-only bench).** `ethctrlmgr` writes ~1.2k lines/s into the 1 MiB main ring (it
holds seconds) and the app's logcat child costs ~3.4 % of a core; the box `wireless` daemon's 1 Hz `EADDRINUSE`
retry loop (F-C1) is 32-37 % of the app's own lines. Raising `persist.logd.size` is a device-owner action; see
`11_HARDENING_PLAN.md`.

### 8.4 Not yet verified on the unit

Probed at start with the result in every file header, not confirmed on hardware: whether an `untrusted_app` may
`exec /system/bin/logcat` under this unit's SELinux policy; which buffers open (`main,system,crash` should;
`events`/`radio` are probed; `kernel` is opt-in and probably refused, `tools/drive_capture.sh` gets `-b all`
only because it runs as the adb shell user); DocumentsUI presence (bench says yes):
`adb shell cmd package resolve-activity -a android.intent.action.CREATE_DOCUMENT -t text/plain`.

Raise the ring once at the bench with `persist.logd.size`, not a runtime `logcat -G` (lost on reboot).
