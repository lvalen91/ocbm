# 06 — Bring-up runbook (device-proven through 2026-08-27)

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
| `CH_MGMT` identity snapshot | done | `CarLink-626a`, serial `2025.02.25.1521626a` |
| `CT_SUBSCRIBE` wakes the box radios | done | `host_present=1`, `hci0 UP RUNNING PSCAN ISCAN` |
| Box raises no SoftAP (`wifi_ap: false`) | done | no `hostapd`, no `wlan0`, moal/mlan not loaded |
| Box spawns no A/V layer | done | `airplayd absent` via `AIRPLAYD_BIN=/bin/true` |
| iPhone completes the full BT handshake | done | see §2 |
| App-supplied hotspot creds reach the `0x5703` source | done | `/etc/hostapd.conf` regenerated from the OCBM config |
| `0x5703` hands the iPhone the VEHICLE hotspot | done | `replying 0x5703 (ssid="myChevrolet 32D4" sec=Wpa2OrWpa3Personal)` |
| iPhone joins `br0` | done | `num_connected_clients=1`, `192.168.5.232` / `32:f0:39:5f:b5:79` |
| iPhone advertises `_carplay-ctrl._tcp` on br0 | done | `Owner iPhone._carplay-ctrl._tcp.local` |
| App has direct unicast reach to the phone | done | ICMP true, TCP 62078 + 49152 OPEN |
| App is the AirPlay endpoint, phone dials in | done | `POST /pair-setup` inbound — §6.1 |
| SRP-6a pair-setup verified against a real iPhone | done | M4/M5 reached — §6.2 |
| Rust core cross-compiles and runs on-unit | done | §6.3 |
| Full A/V session (HEVC + AAC, both stream SETUPs) | done | §7.1 |
| Untethered fault capture (no laptop attached) | done | §7 |

Reproduced on the real truck (`gminfo37`, serial `CJUD4R4f1b5fd0`), not only the emulator.

**The keystone is proven.** `CH_MFI` works from an ordinary app UID — the single most load-bearing
unknown in the architecture.

## 2. The captured BT handshake

Driven entirely by the app's `CT_SUBSCRIBE`, with the box raising no access point:

```
[ssp-agent] loaded 1 persisted link key(s) — bonded phones can reconnect without re-pair
[ssp-agent] DEVICE_CONNECTED 64:31:35:8C:29:69
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
bash tools/build_apk.sh
# -> apk/netprobe-debug-<sha>.apk, with apk/netprobe-debug-latest.apk symlinked to it.
# versionCode is pinned, so the SHA in the filename is the only thing that changes build-to-build —
# always install the symlink, not a literal old filename.
adb install -r -g --user 10 apk/netprobe-debug-latest.apk
# on the truck, install Play-attributed so the in-motion path stays eligible:
adb install -i com.android.vending -r -g --user 10 apk/netprobe-debug-latest.apk
```

The app installs as package `zeno.gmccpa` (the GM USB fixed-handler squat, `android.car.usb.handler`,
was reverted 2026-09-08 — TRUCK-VERIFIED; this is now just the app's own package name). Every
`am`/`pm`/`appops`/`dumpsys` command below targets that package name; source classes are still
`zeno.gmccpa.*`, so activity components are `zeno.gmccpa/zeno.gmccpa.<Activity>`.

`--user 10` is mandatory: a user-0 install does not get the attach dialog and
`ACTION_USB_DEVICE_ATTACHED` routing to fire. There is no silent USB grant anymore — the ordinary
attach resolver and its one-time permission dialog handle it on user 10.

### Run (scriptable — no tapping)

```bash
# framing + state machine, no hardware needed at all
adb shell am start -n zeno.gmccpa/.MainActivity --es run ocbm_selftest

# the real link. NOTE the nested quotes: an SSID with a space is otherwise split by the remote
# shell and every later --es extra is silently swallowed by `am`.
adb shell "am start -n zeno.gmccpa/.MainActivity --es run ocbm_link \
  --es ssid 'myChevrolet 32D4' --es pass '<passphrase>' --es chan '36'"

adb logcat -d -s NETPROBE
```

Other `--es run` verbs (`MainActivity.handleRunExtra`): `full` (BT handoff + Wi-Fi endpoint together —
the real end-to-end sequence, §7.1), `carplay_ui`, `av_sink`, `av_stats`, `display`, `dump_setup`,
`mdns_self`, `carplay_stop`, `ocbm_state`, `ocbm_disconnect`, `ocbm_forget`, `ocbm_stop`,
`capture_status`, `capture_whole_os`, `capture_own`, `export_log`, `export_log_raw` (§7.2).

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
the MAC; sending `DA:BF:58:F4:7F:18` makes iOS parse it base-10 as `0`, look up "receiver 0" among the
peers it browsed, find nothing, and have nothing to dial back to. The correct header for our identity is
`AirPlay-Receiver-Device-ID: 240515366027032`. TXT `deviceid` and `/info` `deviceID` keep the colon MAC —
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
carManager_handlePendingAutoconnect: index = WiFi, for deviceID: B2:D6:2A:9F:C9:30,
    pendingAutoconnectID: B2:D6:2A:9F:C9:30
>>> INBOUND CONTROL CONNECTION from fe80::c53:35fb:7361:372f%br0:58503
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
carManager_handlePendingAutoconnect: pendingAutoconnectID: DA:BF:58:F4:7F:18 is expired (55.682 seconds old)
```

A diagnostic identity swap produced exactly this and looked like a real protocol failure. It was not.

**Caveats on this result:**
- GM's `CarplayService` was force-stopped during the successful run (`:7000` free). Coexistence is
  permanent in the shipping configuration and has since been re-tested (§6).
- The session stopped at `POST /pair-setup`, which was correct and expected at the time — see §6.2.

## 6.2 SRP-6a pair-setup verified against a real iPhone

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
APPID=$(adb shell dumpsys package zeno.gmccpa | sed -n 's/.*userId=\([0-9]*\).*/\1/p' | head -1)
SOCKUID=$((1000000 + APPID))     # user 10 => 10*100000 + appId; on user 0 it is just $APPID
adb shell "cat /proc/net/tcp6" | awk -v u="$SOCKUID" '$8==u && $4=="01"'   # 01 = ESTABLISHED
```

4–5 established sockets to the iPhone's `fe80::…%<if>` plus `POST /feedback` every 2 s = healthy session.
GM's own receiver owns `:7000` (`0x1B58`) under uid `1001000` — as of 2026-08-12 it listens again, so
coexistence is the live condition, not a hypothetical.

Do not leave `nc -l -p 9001` running from manual probing: it steals the seam port from the app and the
receiver's forwarder then connects to `nc` instead of `AvSink`.

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
adb shell am force-stop zeno.gmccpa
adb logcat -c
adb shell "am start -n zeno.gmccpa/.MainActivity --es run full \
           --es ssid 'myChevrolet 32D4' --es pass '123456789000'"
```
Wait for both stream SETUPs before opening the UI:
```bash
adb logcat -d -s NETPROBE | grep -E "SETUP phase2"
#   SETUP phase2 screen(110) ... -> dataPort NNNNN
#   SETUP phase2 audio(102) ... AacLc audioType="media" -> dataPort NNNNN
```

### 2. Open the CarPlay screen
```bash
adb shell am start -n zeno.gmccpa/.MainActivity --es run carplay_ui
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
adb shell am start -n zeno.gmccpa/.MainActivity --es run av_stats
```
`MainActivity` commands are safe during a live session — they detach the video renderer while audio and
the seams keep running; `carplay_ui` reattaches it
(`~/Documents/carlink/old/gm_ccpa/evidence/session_2026-08-05/WORKING_SESSION.md`
§5).

### 4. Restore the screen after anything backgrounds it
```bash
adb shell am start -n zeno.gmccpa/.MainActivity --es run carplay_ui
```
Video returns in ~250 ms via the producer's re-dial + ForceKeyFrame. Audio is never interrupted.

**Do NOT** try `am start -n zeno.gmccpa/zeno.gmccpa.av.CarPlayActivity` — it is
`exported="false"` and `adb shell` (uid 2000) lacks `START_ANY_ACTIVITY` on this user build:
`SecurityException: … not exported from uid 1010123`. Always go through `MainActivity`.

### Verifying without logs (the socket table never lies)
```bash
APPID=$(adb shell dumpsys package zeno.gmccpa | sed -n 's/.*userId=\([0-9]*\).*/\1/p' | head -1)
adb shell "cat /proc/net/tcp6" | awk -v u="$((1000000 + APPID))" '$8==u && $4=="01"'   # 01 = ESTABLISHED
```
4–5 established sockets to the iPhone plus `POST /feedback` every 2 s = healthy session. Derive the uid
— do not hardcode it (§7): a stale literal shows an empty table on a healthy session.

### Troubleshooting

| Symptom | Cause | Action |
|---|---|---|
| App dials, phone answers `HTTP/1.1 200 OK`, no inbound on `:7011` | Stale iOS endpoint cache. iOS logs `carManager_handlePendingAutoconnect: No matching endpoint found for deviceID …` then `-6753 kConnectionErr` | Forget the vehicle on the iPhone (Settings → General → CarPlay) and re-pair over Bluetooth. Confirmed fix — not caused by GM's `:7000` |
| Black screen, session otherwise healthy | video seam bound but no frames | check `[hevc]` lines; `first frame decoded (0 B Annex-B)` means the sample-description unwrap regressed (`01_FINDINGS` §8b) |
| `bind :9001 … EADDRINUSE` | a stale `AvSink` or `nc` holds the seam | `carplay_ui` stops `AvSink` first; kill any manual `nc -l -p 9001` |
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

Observed: the app sent a 153-byte config with `wifi_ssid: myChevrolet 32D4`, the box logged nothing
wrong, and `/etc/hostapd.conf` still read `ssid=ccpa-b0df`. `0x5703` would have handed the iPhone an AP
that is never raised (`wifi_ap:false`), and per §5 gotcha 4 that dead network then poisons the next
attempt until it is forgotten on the phone.

**Fixed:** all three credential entry points (on-screen button, USB-attach, `--es run`) now go through
`MainActivity.applyHotspotFields()`, and `OcbmProbe` refuses to take the radio-wake edge with no SSID
rather than proceeding into an unrecoverable state.

**Recovery if you hit it:** a full `host_present` cycle — `am force-stop`, wait ≥20 s for the flap
detector, then relaunch with `--es ssid/--es pass`. Verify before trusting it:

```bash
bash scratchpad/uart_cmd.sh /tmp/h.txt 11 'cat /etc/hostapd.conf'   # in ccpa_custom root (cd ../.. from here)
tr -d '\r' < /tmp/h.txt | grep -aE '^ssid=|^channel=|^wpa_passphrase='
```
Expect `ssid=myChevrolet 32D4`. If it still says `ccpa-b0df`, the credentials did not reach
`wireless_up`.

### Trap 2 — `NsdManager` serves a stale peer address, and restarting discovery does not fix it

Symptom: the handoff succeeds, the phone joins `br0`, `_carplay-ctrl._tcp` resolves fine — and every
`GET /ctrl-int/1/connect` times out after 3000 ms with no response at all.

The address was dead. `NsdManager` kept answering with an iPhone IPv6 link-local that had rotated away:

```
$ adb shell ip neigh show dev br0
192.168.5.57                 lladdr fe:d8:56:7d:78:18 REACHABLE   <- the phone, actually here
fe80::c0b:7143:d356:a90      lladdr fe:d8:56:7d:78:18 STALE       <- and here
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
half of adapter attaches, and the iPhone refusing to auto-reconnect until iOS forgets the box — only
happen when no Mac is attached. The app captures itself.

`logcat -d` dumps what is already in the ring buffer, so capture does not have to be running when the
fault happens, only within the ring-buffer window afterwards. Capture is armed from
`UsbAttachActivity`, the trampoline the attach resolver launches on every matching USB attach,
and drains the backlog before it starts tailing. A `BootReceiver` widens the window when the process
happens to survive from boot, but coverage does not depend on it.

### 8.1 One-time bench grant

Default scope is own-process, which needs no permission and always works. Whole-OS needs `READ_LOGS`,
which is `signature|privileged|development` — it can never be obtained by declaring it, but the
`development` flag means adb can grant it. The app declares it so the grant has something to land on;
`pm grant` rejects a permission the package does not request.

```bash
adb shell pm grant zeno.gmccpa android.permission.READ_LOGS
adb shell am force-stop zeno.gmccpa       # NOT optional — see below
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
says no. The engine detects exactly this (no foreign pid seen within 15 s), degrades the reported scope,
and prints the force-stop command; believe the `effective` scope, never the granted flag.

The grant survives reboots and `adb install -r`. It is lost only on a full uninstall.

### 8.2 Operating it

```bash
# status: which scope actually took, which buffers opened, bytes on disk, lines dropped
adb shell am start -n zeno.gmccpa/.MainActivity --es run capture_status
adb shell am start -n zeno.gmccpa/.MainActivity --es run capture_whole_os
adb shell am start -n zeno.gmccpa/.MainActivity --es run capture_own
adb shell am start -n zeno.gmccpa/.MainActivity --es run export_log
```

The launcher screen carries the same three on a secondary row: Log Scope, Log Status, Export Logs.

Export walks a ladder and reports which rung it used: SAF document picker → a mounted USB volume via
`getExternalFilesDirs()` → `MediaStore.Downloads` → a no-op that reports the path in `filesDir`. AAOS
frequently ships without DocumentsUI, so the USB-volume rung is the expected workhorse, not the
fallback. `run-as` is blocked on this unit, so this is the only retrieval path that does not need adb.

**Exports are redacted by default.** A whole-OS capture from this unit contains the vehicle hotspot
passphrase, the iPhone's BR/EDR MAC (`CT_PHONE_IDENT.deviceID`), the VIN and GPS; an export lands on a
removable stick and leaves the vehicle. `export_log_raw` opts out and is a separate verb on purpose. The
redactor's known gaps are listed in `LogExport.kt`'s KDoc — read them before trusting an export.

### 8.3 What to grep

Every session emits one `SESSION v=1 …` line plus a `SESSION_DETAIL` block. That is the point: a drive
capture in `evidence/` can run into gigabytes, and reading it is not a diagnostic strategy — diffing one
line per session is.

| Question | Grep |
|---|---|
| Did the USB permission grant land (dialog or cached "always")? | `grep 'USB HANDLER' … held=` and `grep 'prompted=' *.log` — `held=`/`prompted=` are legacy field names from the squat-era diagnostics (`UsbAttachActivity.kt`), kept on purpose post-revert since they still answer the same question about the ordinary attach flow |
| Where does Bluetooth stall? | `grep -o 'btp_max=[A-Z_]*'` — `none`/`LINK_UP` = box never got RFCOMM; `AUTHENTICATING` = MFi; `IDENTIFYING` = iAP2 identification; `WIFI_HANDOFF` = BT succeeded, look at Wi-Fi |
| Bond asymmetry (the "forget it in iOS and it works" shape) | `grep 'bond_changed=true'` |
| Sessions that never produced video | `grep 'no_video=true'` |
| Did the box send something we still don't parse? | `grep 'CTRL unknown type'` |

`desc_fp=` is the adapter's descriptor fingerprint. A changed fingerprint across attaches means the USB
permission cache saw a different device — the leading hypothesis for the intermittent dialog;
`/script/ncm_only` flips the box between descriptor variants.

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
