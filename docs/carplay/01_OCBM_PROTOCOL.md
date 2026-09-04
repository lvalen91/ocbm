# 02 — OCBM: the Open CCPA Bulk Multiplexer

> **STATUS:** CURRENT · the OCBM wire specification, kept in step with `crates/ocbm-proto` and
> `ccpa/ocbmd`

**OCBM is projection-agnostic.** It carries CarPlay and Android Auto over the same envelope — CarPlay
A/V on its own channels, Android Auto as the phone's own byte stream over `CH_IP` — and the channel
map below is the whole of what either protocol needs from the transport. It lives under `carplay/`
only because that is where most of its channels are used; nothing in the envelope is CarPlay-specific.

OCBM is the open replacement for riddleBox's `0x55AA55AA` protocol. Like it, OCBM is a **framed
multiplexer** over the single `/dev/usb_accessory` bulk pipe (one IN stream + one OUT stream): a
stable envelope carries many logical channels, discriminated by a channel id.

The design contract: **the envelope never learns about features.** New codecs, audio formats,
screens, and future phone-OS features are carried as *self-describing data inside channels*, never as
new envelope fields or a new wire version. Android Auto arrived after the envelope was frozen and
needed no change to it.


## The protocol has one definition, and it is checked

`crates/ocbm-proto/src/lib.rs` is the protocol. Every other implementation restates its constants by
hand in another language:

| Implementation | File |
|---|---|
| box daemons (Rust) | `crates/ocbm-proto/src/lib.rs` — **canonical** |
| macOS host app (Swift) | `host/CarPlayHost/carlink_macOS/OCBM/OCBMFraming.swift` |
| Android host app (Kotlin) | `host/CarlinkAndroid/app/src/main/kotlin/com/carlink/ocbm/OcbmProto.kt` |
| gm_ccpa client (Kotlin) | **symlink** to this repo's Kotlin file (see below) |

`tools/proto_check.py` verifies them against the canonical table. Run it before committing a protocol
change, and pass the sibling checkout to include it:

    tools/proto_check.py ~/Documents/carlink/gm_ccpa

A value that disagrees is an error: two ends of one link mean different things by the same byte. A
constant a client has not defined yet is reported as a gap, and is an error only for the core set —
channel ids, `CT_*` opcodes, frame flags — which every client must be able to name even where it does
not act on the opcode. `--strict` promotes every gap to an error.

**gm_ccpa shares the Kotlin file rather than copying it.** As of 2026-08-31
`gm_ccpa/netprobe_app/app/src/main/java/zeno/gmccpa/ocbm/OcbmProto.kt` is a relative symlink to
`host/CarlinkAndroid/app/src/main/kotlin/com/carlink/ocbm/OcbmProto.kt` here, so the protocol is
edited once. The file declares `package com.carlink.ocbm` and gm_ccpa's consumers import it; Kotlin
does not require package to match directory. This adds no new coupling — gm_ccpa's `carplay-jni`
already takes `receiver`, `pairing` and `mfi` as cargo path deps into this checkout.

**Only the protocol file is shared.** The two Android apps serve different roles — gm_ccpa is the GM
head-unit bridge, this repo's app targets Pi AAOS — so `OcbmClient`, `VoiceRouter`, `AacPlayer`,
`MicUplink` and the transports stay separate implementations and are allowed to differ. What must not
differ is what a byte means, and that is what the checker enforces. Deployment policy stays in the
app: gm_ccpa's `BH_REQUIRED_BRIDGE` lives in its `SessionSupervisor`, not in the shared file, because
it deliberately does not require `BH_WLAN_AP` in a role where the head unit owns Wi-Fi.

**Why this exists.** `CT_PROJ_MODE`, `CT_BOX_HEALTH`, the `BH_*` health bits, `F_REPLAY` and the whole
`CH_FILE` opcode set were added to the box and picked up by the gm_ccpa client, while this repo's own
Swift and Kotlin clients never learned them — as was the MFi correlation tag, which `ocbmd` echoes and
gm_ccpa's client sends, but which existed nowhere in `ocbm-proto` or in either client here — the macOS app could not name `CT_BT_PHASE` or
`CT_BOX_HEALTH` at all, so box health telemetry arrived and was ignored. That gap was closed on
2026-08-31 and this checker is what keeps it closed. Adding a constant to `ocbm-proto` without adding
it to the clients now fails the check.

## Envelope (v1)

16-byte header, little-endian, on both bulk directions:

```
off 0:  magic    u32   = 0x4F43424D  ("OCBM")   ; resync marker
off 4:  length   u32                             ; payload byte count
off 8:  channel  u16                             ; logical channel (the "type")
off 10: flags    u8                              ; bit0 SOM, bit1 EOM (fragment), bit2 REPLAY, bit3 NEW_SOURCE
off 11: hcheck   u8                              ; XOR of header bytes 0..=10 (resync robustness)
off 12: seq      u32                             ; per-endpoint sequence (debug/telemetry — see below)
off 16: payload  [length]
```

Properties: **resyncable** (scan for `magic`, verify `hcheck`), **fragmentable** (SOM/EOM for
messages larger than a comfortable transfer), and **skippable** (`length` lets any receiver skip
unknown content). Bulk is a byte stream, so all message boundaries are OCBM's, not USB's.

**`F_REPLAY` (bit2, added 2026-08-27):** set by the box on a box→host state-mirror frame emitted
because the box had no prior value for that state — a fresh `CT_SUBSCRIBE`, or the first read after
an `ocbmd` restart — rather than because the state changed. It answers the one question a re-emitted
mirror cannot otherwise answer: *is this news, or is this what I already knew?* A mirror sourced from
a flag file that is never cleared otherwise replays as a live event on every reattach. Purely
advisory, and **no receiver on either end validates flag bits**, so the two sides may adopt it
independently and in either order. Reasoning: [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-02-4`.

**`F_NEW_SOURCE` (bit3, added 2026-09-03):** set by the box on the FIRST frame it forwards from a
newly accepted A/V seam connection (`:9001`–`:9005`). A re-SETUP reconnects the seam and ocbmd
replaces the previous producer *without draining it*, so the host's byte-stream reassembly for that
channel may still hold a partial message from the old producer; this bit says "drop that remainder,
my bytes start at a message boundary". It is connection-lifecycle knowledge ocbmd already has — the
seam payload itself is forwarded untouched, so it does not breach *the box forwards, the app
processes*. Advisory exactly like `F_REPLAY`. See the audio-lane correction below for what it fixes.

**`seq` as implemented:** each endpoint keeps **one global counter across all channels** —
monotonically increasing (wrapping), stamped on every frame it sends, reset to 0 by ocbmd on a new
HELLO. It is **debug/telemetry only; no receiver validates it** (`Reassembler::next` doesn't even
surface it). A per-channel *validated* sequence is a possible future hardening: it would make the
splice after an OOM-backstop queue clear (out_hi / out_lo cap hit) detectable at the receiver
instead of silent.

> New magic vs. reusing `0x55AA55AA`: a new magic keeps OCBM cleanly distinct from stock so the
> two can never be confused. Reusing the stock envelope would reduce changes to a `carlink_native`
> that already parses it — a transition-compat option, not the default.

## Handshake (CTRL channel, first thing after the host claims the interface)

```
host → box:  HELLO   { ocbm_version, host_instance_nonce }
box  → host: HELLO_ACK { ocbm_version, box_caps, active_mode }
host → box:  MODE_SELECT { mode }        (only when the host wants a non-default mode)
```

- `HELLO_ACK`'s `box_caps` mirrors CarPlay's own negotiation (see Extensibility) so the two ends
  evolve independently; unknown capabilities are ignored. There is **no host→box capability word** —
  that slot is the host instance nonce (see the note under the CTRL table). A host's capability is
  expressed by which channels and opcodes it actually uses.
- **Optional host label** (added 2026-08-27). Anything after the nonce in `CT_HELLO` is an optional
  UTF-8 label naming *what kind* of host this is — the box serves several (a GM head-unit app using it
  as a BT+MFi bridge, an Android Auto host driving `aa-bridge` over `CH_IP`, bench tooling) and its
  behaviour and logs differ between them, yet "what is talking to me" was previously unanswerable. The
  box filters control characters, caps it at 64 chars, logs it, and reports it in `MGMT_INFO` as
  `host_name`. Purely diagnostic — nothing gates on it, and an absent label reads exactly as before.
  Additive in both directions: the box already ignored bytes past the nonce, and an older box still
  does.
- `mode` is host-selected. **Branch as early as possible** — before any projection machinery —
  so a rescue mode shares almost no fate with the CarPlay stack. As implemented, the box answers
  `HELLO` immediately, so `HELLO_ACK`'s `active_mode` reports the mode in force *at HELLO time* —
  `PROJECTION` on a fresh link, `CONSOLE` only if an earlier `MODE_SELECT` on this same ocbmd already
  switched it (the PTY outlives a host detach; it is dropped only when the console shell exits).
  It is **not** an acknowledgement of your own `MODE_SELECT`, which is a separate handler arm that
  never answers on CH_CTRL — its only reply is the `[ocbmd] CONSOLE attached (root)` banner on
  `CH_CONSOLE`.

### CTRL message types (first payload byte on CH_CTRL = `0x0000`)

> **CORRECTED 2026-08-27:** `0x19` `CT_PROJ_MODE` had no row anywhere in this document and a stray
> blank line split the table in two below `0x18`; the opcode range quoted under "Self-describing
> streams" still read `0x01`-`0x18`. Both fixed in place — [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-02-3`.

| Byte | Message | Direction | Payload after the type byte |
|---|---|---|---|
| `0x01` | `HELLO` | host→box | `[ver u8][instance u32 LE]` — the trailing u32 is the host **instance nonce**, not caps. See the note below the table |
| `0x02` | `HELLO_ACK` | box→host | `[ver u8][caps u32 LE][active_mode u8]` |
| `0x03` | `MODE_SELECT` | host→box | `[mode u8]` |
| `0x04` | `SRC` | host→box | `[ms u32 LE]` — box floods CH_ECHO for a downlink benchmark. The flood blocks ocbmd's single-threaded poll, so the box **clamps `ms` to `HEARTBEAT_GRACE/2` (5 s)** and refreshes `last_hb` when it ends (`Daemon::handle`, the `CT_SRC` arm). An unclamped bench past the grace used to make `presence_tick` see a stale `last_hb`, declare the host GONE and destroy the live session — a host asking for 30 s gets 5 s, not an error |
| `0x05` | `SETTIME` | host→box | `[unix_seconds u64 LE]`; box `settimeofday()`s, then acks `[0x05][unix_seconds u64 LE][status u8]` (`0`=applied, `1`=failed) | (2026-09-03: the macOS client now sends it right after every SUBSCRIBE — until then only the Android client did, and a box that booted unsynced logged every line, read-time and write-time stamped alike, as 2020-01-02)
| `0x06` | `CT_ETH_START` | host→box | `[iface bytes?]` — box bridges that netdev (default `ncm0`) onto CH_ETH |
| `0x07` | `CT_ETH_STOP` | host→box | *(empty)* — box tears the raw-frame bridge down |
| `0x14` | `CT_UPLINK` | box→host | `[state u8][rate u32 LE][ch u8][codec u8]` — mic-uplink gate: `1`=on (iPhone opened a type-100 `input=true` SETUP, or an HFP call opened SCO; app starts capturing at rate/ch), `0`=off (TEARDOWN; app stops). `codec` added 2026-09-04: `0` PCM S16LE (every CarPlay uplink and HFP/CVSD), `4` mSBC — the app returns whole 60 B eSCO packets, not PCM. ON is 8 bytes; **OFF stays the 7-byte all-zero form**, so read `codec` only when `len ≥ 8` and default it to 0 |
| `0x15` | `CT_PAIRING_CODE` | box→host | `[6 ascii digits \| empty]` — the wireless SSP Numeric-Comparison code to display for the user to match against the iPhone; empty payload = clear/hide | (macOS app, 2026-09-03: rendered one digit per shaded cell in the main-window overlay, grouped n/2 + n/2 with a dash for even lengths ≥ 4, semantic system colours, VoiceOver label; hidden when the payload is empty. The one-line status only carries the instruction.)
| `0x1C` | `CT_PAIR_CONFIRM` | host→box | `[accept u8]` — the USER'S answer to the `CT_PAIRING_CODE` prompt: `1` = the codes match, pair; `0` = cancel. Any non-zero byte is a yes; a truncated frame reads as **cancel** (an unparseable request must never complete a bond nobody confirmed). SSP Numeric Comparison requires a real yes/no on BOTH devices, so the box no longer auto-accepts: it publishes the code, waits up to **55 s** (inside `pairing_aware_connect`'s 60 s hold), then replies `USER_CONFIRM_REPLY` or `USER_CONFIRM_NEG_REPLY`. ocbmd relays this to carplay-wireless's control port as `{"cmd":"pair_answer","accept":…}` (127.0.0.1:9115). An answer with no prompt outstanding is ignored. (macOS app, 2026-09-03: **Pair** default/Return and **Cancel**/Escape under the code panel, disabled after one answer until the box clears the code.) See docs/wireless/01_BT_AND_RADIO.md |
| `0x16` | `CT_RADIO` | host→box | `[0 \| 1]` — `0` = radios off now, `1` = clear the inhibit (the pushed cfg still governs). Any other value is logged and ignored; a fresh `CT_SUBSCRIBE` clears the inhibit **unconditionally**, so a config push always overrides a prior `off`. See the note below before trying to change that |
| `0x17` | `CT_BT_PHASE` | box→host | `[BTP_* u8]` — Bluetooth/iAP2 handshake progress. See below |
| `0x18` | `CT_PHONE_IDENT` | box→host | `[utf8 JSON \| empty]` — who the connected phone is: `{"name","deviceID","model","osName","osVersion"}`, lifted from the phone's own AirPlay phase-1 SETUP plist. `deviceID` is the BR/EDR MAC, so it joins `MGMT_INFO`'s bonded list. Sent only while subscribed, on **change** only, re-emitted after each `CT_SUBSCRIBE`; empty payload = no identity yet / cleared |
| `0x19` | `CT_PROJ_MODE` | box→host | `[PM_* u8]` — WHICH projection transport owns the box right now: `0x00` `PM_NONE` (idle), `0x01` `PM_WIRED_CP`, `0x02` `PM_WIRELESS_CP`, `0x03` `PM_WIRED_AA`, `0x04` `PM_WIRELESS_AA` (reserved, unbuilt). Mirrors the box's single-owner arbitration flag `/tmp/projection_owner` (docs/androidauto/02_ARBITRATION.md). Sent only while subscribed, on **change** only, re-emitted after each `CT_SUBSCRIBE`. Advisory: an unknown value means "some transport owns the box" — never gate on ordering |
| `0x1A` | `CT_BOX_HEALTH` | box→host | `[BH_* bitmask u8]` — the box's own readiness. Sent only while subscribed, on **change** only, re-emitted after each `CT_SUBSCRIBE`. See below |
| `0x1B` | `CT_LOG_CTL` | host→box | `[enabled u8][cap_kb u16 LE]` — arm/disarm the box→host `CH_LOG` stream. `cap_kb` 0 = the built-in default (256). **Default is OFF**, and it resets to off on `CT_STOP` / host-gone like every other per-session state. Enabling streams from **offset 0** of every source — that IS the backfill, there is no separate dump opcode — then follows EOF. Every line that existed on disk at the moment a source is (re)opened for that offset-0 pass carries `LOG_F_BACKFILL` (see §CH_LOG), so a host can tell "already happened" replay from what happens next. A payload shorter than 2 bytes reads as *disable*: an unparseable request must never leave a stream running the host does not know about. See §CH_LOG |

### `CT_BOX_HEALTH` (`0x1A`) — the box's own readiness (added 2026-08-27)

| Bit | Name | Meaning |
|---|---|---|
| `0x01` | `BH_HCI_PRESENT` | `hci0` exists **and is UP** (`HCI_UP` in `/sys/class/bluetooth/hci0/flags`) |
| `0x02` | `BH_SSP` | Secure Simple Pairing enabled on `hci0` |
| `0x04` | `BH_IAP2D` | `iap2d` running (wired CarPlay identify path) |
| `0x08` | `BH_AIRPLAYD` | `airplayd` running |
| `0x10` | `BH_CARPLAY_WIRELESS` | `carplay-wireless` running |
| `0x20` | `BH_WLAN_AP` | `hostapd` running — the box is raising its **own** AP |
| `0x40` | `BH_ROOTFS_OK` | rootfs has headroom (≥5% or 2 MB) |

**Why it exists.** Until this, the only way a host could learn anything about the box's health was to
ask — `MGMT_GET_INFO`, a JSON snapshot returned on request and nothing else. Hosts asked once at
bring-up and never again, so a box whose `hci` went away or whose `carplay-wireless` died mid-session
was indistinguishable from a healthy one. A host cannot evaluate "am I ready **and** is the box ready"
against a snapshot it took minutes ago.

**A bitmask, not JSON, on purpose.** This is on a change-triggered path that can fire during live A/V;
it has to be cheap enough never to think twice about. `MGMT_INFO` remains the place for detail
(identity, bonded list, free space).

**`BH_HCI_PRESENT` — read this bit first when Bluetooth "does nothing".** It now reads `HCI_UP` from
`/sys/class/bluetooth/hci0/flags`, so it reflects the radio's actual power state.

> CORRECTED 2026-08-29, TWICE — the second correction is the one that stands.
>
> It originally tested only that the sysfs NODE existed, which survives `hciconfig hci0 down` —
> exactly how `wireless_down` takes Bluetooth down, since the module is deliberately left attached.
> So the bit did not clear on a mid-session hci-down, the very case it would be most useful for.
>
> The first fix read `HCI_UP` from `/sys/class/bluetooth/hci0/flags` and this section said so.
> **That file does not exist on this box.** The 3.14 kernel's `hci0/` exposes exactly `address
> device name power subsystem type uevent`; the read failed every time and reported "no controller"
> against an `hci0` that was `UP RUNNING` — strictly worse than the node-exists test it replaced,
> and wrong in the most misleading direction, since a clear bit is what this section tells you to
> trust. Caught only by putting it on hardware.
>
> It now uses **`HCIGETDEVINFO`** on a raw HCI socket — the same ioctl `hciconfig` itself uses. One
> `socket` + `ioctl` + `close` per 2 s health tick, no fork and no blocking, which is what ocbmd's
> single-threaded dispatch loop can afford (it also carries the MFi relay and the heartbeat; forking
> `hciconfig` there is what the SSP note already rules out). Verified on hardware both directions:
> `hci0 UP RUNNING` -> bit set; `hciconfig hci0 down` with the node still present -> bit clear.

A **clear** bit still covers the case that matters most, and it is the one that actually bit us: no
controller registered at all. `hci_uart` is a loadable module on the CCPA, and if nothing `insmod`s it
the `n_hci` line discipline is never registered, `hciattach` fails `EINVAL`, and `hci0` is never
created — while the OCBM claim, the MFi relay, `CT_SUBSCRIBE` and `HOST_PRESENT` all still report
success. A health of `0x50` (`carplay-wireless|rootfs-ok`) with bit 0 clear is that exact signature.
See `../ops/06_CORRECTIONS_LEDGER.md` R-20W-5.

**Cost discipline.** The tick returns immediately when `!subscribed`, so an idle box does no work. SSP
is sampled **once per session**, not per tick: `ssp_enabled_cached()` spawns `hciconfig` behind a 30 s
TTL, and calling it from a 2 s tick would guarantee a fork every 30 s for the life of every session, on
the single-threaded dispatch loop, with a 2 s stall if `hciconfig` ever wedged.

### `CT_RADIO` and `CT_SUBSCRIBE`: the inhibit clear is unconditional

A fresh `CT_SUBSCRIBE` always clears the radio inhibit. Holding it across a same-host/same-cfg
resubscribe was implemented and reverted on 2026-08-27 — it was inert where it was wanted and stranded
the box where it fired. **A host that wants radios down after a reattach re-asserts `CT_RADIO 0`
itself**; an inhibit a host sets is an inhibit that host must release. Reasoning:
[../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-02-1`.

The session-control opcodes `0x10` `CT_SUBSCRIBE`, `0x11` `CT_STOP`, `0x12` `CT_HEARTBEAT` and
`0x13` `CT_SESSION_EVENT` are deliberately not repeated here — they are specified in the session
lifecycle section below.

> **`HELLO`'s trailing u32 is the host INSTANCE NONCE, not capabilities.** The box has never read
> capabilities from `HELLO` — not in the current handler (the `CT_HELLO` arm of `Daemon::handle` in
> `ccpa/ocbmd/src/main.rs`) nor in the
> oldest one in history. It reads those four bytes as an opaque identifier for the host *process*:
> non-zero, fixed for the lifetime of one host session object, re-sent on every reattach that
> session makes. `0` = not supplied, which the box treats exactly as it did before the field had a
> meaning (the `pl.len() >= 6` and `inst != 0` guards in the `CT_HELLO` arm of `Daemon::handle`
> — a zero or short nonce falls past both and leaves `host_instance` untouched);
> a `HELLO` shorter than 6 bytes is tolerated the same way.
> A **different** nonce arriving while the box still holds a previously-recorded nonce **and** still
> believes a host is present means the previous host died without `CT_STOP`: the box sets
> `host_replaced`, and the next `CT_SUBSCRIBE` turns it into a silent 2 s dip of `/tmp/host_present`
> (`Daemon::rearm_presence_silently`, which dips the flag and holds it for
> `REARM_HOLD` = 2 s) so the supervisor respawns airplayd. The host is told
> `SEV_HOST_PRESENT` only, never `SEV_HOST_GONE` — sending GONE there was measured to tear
> projection down. The **same** nonce is that host reattaching, which sets nothing: `CT_SUBSCRIBE`
> then takes the ordinary path (a fresh `SEV_HOST_PRESENT`, or the plain `set_present(true)` edge if
> presence had already dropped). A predecessor that sent `CT_STOP` is never a replacement — it went
> fully idle, so `present`/`subscribed` are both false when its successor's `HELLO` arrives.
> Only `HELLO_ACK`'s u32 is a capability bitmask.
>
> **Conformance (2026-08-16, updated 2026-09-01).** The Android host supplies a real nonce
> (`CarlinkManager.kt:360`, random per manager, never 0, held stable across the clients it builds).
> `ocbm-host` now does too: `host_instance_nonce()` in `host/ocbm-host/src/main.rs` derives a
> per-process value (pid ⊕ wall-clock nanos, forced non-zero) once and re-sends it, so the bench
> tool exercises replacement detection instead of defeating it. Until 2026-09-01 it sent the
> caps-era constant `0x13`, which was worse than 0 — two successive runs looked like one host to the
> box, so the re-arm never fired.
> `OCBMClient.swift:197` still sends zeros — legal, but it forfeits replacement detection on macOS,
> and its comment calling the field "caps" is stale. That remains a client bug against this spec,
> not an alternative reading of it.


### `CT_BT_PHASE` (`0x17`) — Bluetooth/iAP2 handshake progress

**Why it exists.** The host is not in the Bluetooth loop at all: the box owns the radio, runs the
whole iAP2 handshake, and answers the `0x5702`/`0x5703` Wi-Fi handoff on its own. `SEV_PHONE_*` do
**not** help — they report an iPhone on the *box's own USB bus*, which in a wireless-only deployment
never happens. So between `CT_SUBSCRIBE` and the phone appearing on Wi-Fi — the longest phase of the
whole session — a host application had **no signal whatsoever**, and could only poll `/tmp/wl.log`
over a debug serial console. A host that wants to prepare for projection (advertise its Wi-Fi
endpoint, stand its A/V consumers up, show progress UI) had nothing to trigger on but a timer.

| Value | Name | Meaning |
|---|---|---|
| `0x00` | `BTP_IDLE` | no BT session in progress |
| `0x01` | `BTP_LINK_UP` | RFCOMM/iAP2 link established (SYN-ACK) |
| `0x02` | `BTP_AUTHENTICATING` | MFi cert/challenge exchange under way (`0xAA01`/`0xAA03`) |
| `0x03` | `BTP_AUTHENTICATED` | `0xAA05` AuthenticationSucceeded |
| `0x04` | `BTP_IDENTIFYING` | `0x1D01` IdentificationInformation sent |
| `0x05` | `BTP_IDENTIFIED` | `0x1D02` accepted — the phone has the accessory |
| `0x06` | `BTP_WIFI_HANDOFF` | `0x5703` sent: the phone now has the hotspot credentials |

**Transport.** `wireless::bt_driver::publish_bt_phase` writes the value to `/tmp/bt_phase`; `ocbmd`'s
`bt_phase_tick` mirrors it to the host on change. This is deliberately the **same mechanism as
`CT_PAIRING_CODE`** rather than a new IPC path: the BT driver holds no handle on the OCBM link (it is
a different process), the flag is last-write-wins so a missed update is self-correcting, and a write
failure can never perturb the handshake it is reporting on.

**Contract for hosts.**
- **Advisory. Never gate on ordering.** Treat an unrecognised value as "progress" and ignore it. A
  reconnect, a retry, or a future firmware may skip or repeat phases; a host that requires a strict
  sequence will hang on hardware that is working correctly.
- **Emitted on change only**, throttled to 500 ms once latched, and **re-emitted on host re-attach**
  so an app that subscribes mid-handshake is not left blind until the next transition (which may
  never come — `BTP_IDENTIFIED` can be the last one for minutes).
- `BTP_WIFI_HANDOFF` is the actionable one for a Wi-Fi receiver: it means the phone has the
  credentials and is about to leave Bluetooth, so the endpoint should already be advertised and the
  A/V consumers already listening.
- Absent or unparseable `/tmp/bt_phase` reads as `BTP_IDLE`; a torn read is never forwarded.

**Compatibility.** Purely additive. An older host ignores an unknown `CT_*` opcode (the CH_CTRL
handler's `else` arm logs and drops it), and an older box simply never sends it — a host must
therefore treat "no `CT_BT_PHASE` ever arrives" as normal and keep whatever fallback it had.


### The `:9003` voice seam tag (legacy / on-box-decrypt mode)

Specified here because it is the mode `gm_ccpa` actually runs (`OCBM_FWD_ENC=0`), and it was
previously described only in a source comment. **This is not the seam-v2 framing** — that is the
`[u32 BE len][SEAM_MAGIC][marker]` envelope with `SEAM_KEY`/`SEAM_FORMAT`/`SEAM_PKT` documented **below**
(§"Media transport — committed model"), used when
forward-encrypt is on. The two are wire-incompatible and there is **no discriminator on the wire**:
mode is chosen out-of-band by `OCBM_FWD_ENC` and a consumer must know which it is receiving.

`:9002` (media) carries bare ADTS for AAC-LC, or raw samples for PCM — no framing at all.
`:9003` (everything else) tags every access unit:

```
[rate u32 BE][ch u16 BE][atype u8][len u32 BE][AU]      // 11-byte header
```

`atype` is the CarPlay audio **purpose**, from the SETUP dict's `audioType`:

| `atype` | `audioType` | Notes |
|---|---|---|
| 0 | `media` | routed to `:9002`, so it should never appear on `:9003` |
| 1 | `telephony` | phone-call audio |
| 2 | `speechRecognition` | |
| 3 | `alert` | |
| 4 | `default` / absent | Siri downlink (type 100) or alt-audio (type 101) — distinguish by format |
| 5 | `compatibility` | a **PCM media fallback**, not a voice stream |

**Why the byte exists.** `:9003` multiplexes every non-media stream onto one socket, and the legacy
tag carries no `scid`. Telephony, speechRecognition and the Siri `default` downlink are all
negotiated as AAC-ELD 16 kHz mono, so without `atype` they are byte-for-byte indistinguishable —
`(rate, channels)` yields one bit where five purposes are needed. A consumer forced to guess routes
call audio to the assistant output, which on an Android Automotive head unit means the wrong volume
group and a call whose volume the user cannot adjust.

**`compatibility` is deliberately its own value.** It used to fall into the `default` arm, but it is a
media-carrying PCM fallback: at 48 kHz stereo it is indistinguishable from alt-audio/navigation, so a
consumer routing `atype 4` by format would send it to the nav output and feed PCM to an ELD decoder.

> **Header length changed 10 → 11 bytes** when `atype` was added. A consumer still expecting 10 reads
> `len` from the wrong offset and gets a value in the 16.7–83.9 M range (because `atype` is 1–5 on this
> seam), so the failure is loud — a length-guard trip or a hang, not silent corruption. That is a
> property of the value range, not a designed-in guard: there is no magic or version byte on this seam.

**Session-control opcodes (host-presence, see [`../carplay/02_SESSION_LIFECYCLE.md`](../carplay/02_SESSION_LIFECYCLE.md)).**
On top of the above, CH_CTRL carries the session-control opcodes that gate the whole projection
lifecycle: `CT_SUBSCRIBE` (`0x10`, host→box, + ephemeral YAML config), `CT_STOP` (`0x11`),
`CT_HEARTBEAT` (`0x12`), and `CT_SESSION_EVENT` (`0x13`, box→host, `SEV_HOST_PRESENT` `0x01` |
`SEV_HOST_GONE` `0x02`). `CT_SESSION_EVENT` also mirrors *phone* presence on the box's phone-facing
bus — `SEV_PHONE_PRESENT` `0x03` | `SEV_PHONE_ABSENT` `0x04` — so the app can show a truthful
"waiting for phone" immediately. **`CT_STOP` ends the session immediately and completely** (changed
2026-09-03): it is a session-end indicator, so ocbmd takes the identical `go_idle` teardown a lost
heartbeat takes — presence drops to 0 on `/tmp/host_present` in the same instant, the supervisor runs
its full wireless teardown back to IDLE and the phone disconnects. There is no longer a warm-reuse
grace; the only thing `CT_STOP` does differently from heartbeat loss is *not* send `SEV_HOST_GONE`
back (the host has already detached, and that frame would be the first thing the NEXT host reads).
The `CT_SUBSCRIBE` that follows gets `SEV_HOST_PRESENT` immediately, but the box holds the *flag*
raise until that GONE edge is `REARM_HOLD` (2 s) old, so a quit→relaunch landing inside one 1 Hz
supervisor sample cannot hide the teardown from it.
ocbmd tracks presence, runs a **10 s** heartbeat watchdog
(`HEARTBEAT_GRACE` — deliberately **widened from 3 s** per audit QC #428: expiry is maximally
destructive, and a macOS host can miss several ~1/s beats to App Nap or a brief USB stall without
the session being dead), and mirrors presence to **`/tmp/host_present`** — the cross-process signal
the supervisor reads to drive IDLE→projection→ARM / TEARDOWN. The config is held only for the
session and **never persists**; only pairing persists (disk-backed PeerStore).

**SETTIME / clock sync (no RTC battery).** The CPC200-CCPA has no RTC backup battery, so its
wall clock is bogus at every boot — yet CarPlay/AirPlay TLS pairing requires a valid clock. The
box's `sync_box_time.sh` (NTP) only works when there is an internet route, which the shipped
**OCBM-only** head-unit link does not provide. So `ocbm-host` **auto-pushes the host's wall clock
over CH_CTRL right after HELLO** on every connect, and the box applies it. This is the primary
clock source for the deployed design; NTP is a fallback for the rare connected case.

### Modes (a small, extensible state machine)

| Mode | Meaning |
|---|---|
| `PROJECTION` *(default)* | Full CarPlay path (committed model: box does pairing → forwards encrypted A/V + session key; app decrypts/decodes) |
| `CONSOLE` | Wire the accessory pipe straight to a root PTY; **no** MFi/radios/receiver. See [`../ops/01_RECOVERY.md`](../ops/01_RECOVERY.md) |
| `FIRMWARE_UPDATE` *(future)* | Self-hosted flasher — push a new rootfs/`mtd2` over the same pipe |
| `LOG_DUMP` / `FACTORY_TEST` *(future)* | Diagnostics |

Default is `PROJECTION`, so a stock/unknown host never lands in a shell by accident; only our host
explicitly requests `CONSOLE`. New modes are pure additions — no rewrite.

## Channels (v1)

| Id | Channel | Payload | Model |
|---|---|---|---|
| `0x0000` | CTRL | handshake / caps / session lifecycle / box→host status (the `CT_*` table above; **no stream open** — see §Self-describing streams) | all |
| `0x0001` | MFI | `[op u8][plen u16 BE][payload]` (verbatim bridge frame) | all |
| `0x0002` | CONSOLE | raw PTY byte stream (root shell) | rescue |
| `0x0010` | IP | `[type u8][conn_id u16 LE][data]` stream-mux (userspace L3/L4 relay) — **diagnostic/utility** | — |
| `0x0011` | FILE | `[type u8][…]` — verified host↔box file push / pull (binary deploy) | all |
| `0x0012` | ETH | one raw L2 ethernet frame bridged from a box netdev — **diagnostic** | — |
| `0x0020` | VIDEO | box→host: main-screen video stream (key handoff + forward-encrypted frames; box seam `:9001`) | projection |
| `0x0021` | MEDIA_AUDIO | box→host: media-audio streams (box seam `:9002`) | projection |
| `0x0022` | ALT_AUDIO | box→host: voice-sink streams — telephony / speechRecognition / alert / default (box seam `:9003`, same seam framing as media) | projection |
| `0x0023` | METADATA | box→host: session metadata, **plaintext** (box seam `:9004`; `[u32 BE "META"][u32 BE len][marker][payload]` v2 — the magic lets the host resync after a truncated frame, audit Fix #17 — with `META_CMD 0x01` / `META_JSON 0x02` / `META_ARTWORK 0x03` / `META_CORNERMASK 0x04` `[u32 BE display_width_px][PNG]`, iOS's own `topLeftCornerMask`, docs/carplay/06_AV_PIPELINE.md) | projection |
| `0x0024` | ALT_VIDEO | box→host: the ALT / navigation (instrument-cluster) screen stream, decoded host-side on a **dedicated** decoder (box seam `:9005`) | projection |
| `0x0030` | INPUT | host→box: **binary `INPUT_*` sub-frames** — `INPUT_TOUCH 0x01` (normalized u16 coords) / `INPUT_KEYFRAME 0x02` / `INPUT_MEDIA_BTN 0x03` (Consumer-Control HID uid 2) / `INPUT_COMMAND 0x04` / `INPUT_NAV 0x05` (D-Pad HID uid 3, `NAV_*`) / `INPUT_KEYFRAME_ALT 0x06` (re-IDRs the ALT/cluster stream **specifically**; a bare `INPUT_KEYFRAME` only re-IDRs main) / `INPUT_KNOB 0x07` (Knob HID uid 4) / `INPUT_TELEPHONY 0x08` (Telephony HID). `INPUT_COMMAND`'s payload is the `CMD_*` set — `CMD_REQUEST_UI 0x01`, `CMD_REQUEST_SIRI 0x02` *(deprecated, iOS ignores it)*, `CMD_SIRI_DOWN 0x03` / `CMD_SIRI_UP 0x04`, `CMD_NAV_START 0x05` / `CMD_NAV_STOP 0x06` / `CMD_NAV_CARD 0x07` / `CMD_NAV_APP 0x0A`, `CMD_LIMITED_UI_ON 0x08` / `CMD_LIMITED_UI_OFF 0x09`, `CMD_NAV_APPEARANCE 0x0B`, `CMD_NAV_ZOOM_IN 0x0C` / `CMD_NAV_ZOOM_OUT 0x0D`, `CMD_UI_APPEARANCE 0x0E` / `CMD_MAP_APPEARANCE 0x0F` / `CMD_NIGHT_MODE 0x10`. ocbmd relays every sub-frame opaquely to airplayd; airplayd taps the iPhone HID devices for touch/media/nav/knob/telephony, dispatches `INPUT_COMMAND` as an AirPlay `/command`, and turns `INPUT_KEYFRAME` / `INPUT_KEYFRAME_ALT` into a `forceKeyFrame` on the event channel (main / `VideoStream.Alt1`). Constants are authoritative in `crates/ocbm-proto/src/lib.rs` | all |
| `0x0031` | MIC | host→box: mic-uplink PCM (S16LE at the CT_UPLINK-negotiated rate/ch); ocbmd relays to airplayd's mic-ingest seam → RTP uplink to the iPhone | projection |
| `0x0040` | MGMT | box management, request/response (the app's "CCPA" tab): host→box `MGMT_GET_INFO 0x01` / `MGMT_REBOOT 0x02` / `MGMT_FORGET_ALL 0x03` / `MGMT_FORGET_DEVICE 0x04` / `MGMT_RESTART_WIRELESS 0x05` / `MGMT_ENTER_NCM 0x06` (added 2026-09-03: box arms the persistent `/script/ncm_only` flag, drops any `/script/ocbm_trial` dead-man, ACKs, reboots into NCM maintenance mode; sticky — return over ssh with `rm /script/ncm_only; reboot`. Reachable from the app's CCPA tab (confirmed) and from `open -a <carlink_macOS.app> carlink://box/enter-ncm` while the app holds the USB interface (the `-a` form is required for a bundle in a build directory: LaunchServices does not bind the `carlink` scheme for it, so a bare `open carlink://…` fails with kLSApplicationNotFoundErr)); box→host `MGMT_INFO 0x81` / `MGMT_ACK 0x82` | all |
| `0x0041` | RTSP | box↔host: the **app-driven SETUP relay** (box seam `:9106`, `receiver::relay`). ocbmd is a dumb byte pipe; the endpoint framing is `[u32 BE "RTSP"][u32 BE len][msg]` (len ≤ 512 KiB, magic-resync) carrying the `RS_*` messages below. Rides **out_hi** with the control plane (timing-critical pair/SETUP/RECORD phase) | projection |
| `0x0042` | LOG | **box→host only:** the box's own logs, streamed live. Payload = one or more back-to-back entries, `[source u8][flags u8][seq u16 LE][unix_ms u64 LE][len u16 LE][text]`, packed up to a 4096 B payload per frame. Off until the host sends `CT_LOG_CTL`. See below | all |
| `0x00FF` | ECHO | benchmark echo / CT_SRC flood target | all |
| `0x0FFF` | DISCARD | box parses + drops silently (uplink benchmark sink) | all |
| `0xF000–0xFFFF` | reserved: **experimental / vendor** | — | — |

### Media transport — committed model (encrypted forward + session-key handoff) — VALIDATED

In the committed architecture the box does the AirPlay **pairing** and derives the ChaCha20 keys, then
forwards the **encrypted** media untouched and hands the app the **ephemeral session key**; the app
decrypts + decodes. So the media that crosses OCBM is **ciphertext**, not decoded typed streams.

**Implemented + hardware-validated (2026-07-10; four lanes since).** Two VIDEO lanes — **`0x0020`
CH_VIDEO** (main, box seam `:9001`) and **`0x0024` CH_ALT_VIDEO** (cluster, `:9005`) — and two AUDIO
lanes — **`0x0021` CH_MEDIA_AUDIO** (`:9002`) and **`0x0022` CH_ALT_AUDIO** (voice, `:9003`). Each lane
is a byte stream of length-prefixed messages, **but the two families do NOT share a framing** — they
evolved separately, and a host must implement both.

**ocbmd does not preserve message boundaries.** It reads up to 64 KiB off the box seam and emits it as
one OCBM frame with `F_SOM|F_EOM` always set and the daemon-global `seq`. On the media channels the
OCBM flags and seq therefore say nothing about seam messages: reassemble by the seam length prefix
across frames — a 4K IDR is ONE message of several MB spanning many frames — and never cap a message at
`MAX_PAYLOAD`.

**Video lanes** (`receiver::session::forward_screen2`). Envelope
`[u32 BE len][SEAM_MAGIC "SEAV" 4B][marker u8]…`, `len` counting every byte after itself (magic
included). Two messages:
- key   — `[len=45]["SEAV"][0x00][key.output 32B][scid u64 LE]`
- frame — `[len=141+body]["SEAV"][0x01][seq u64 LE][hdr 128B][body]`

`hdr` is the plaintext `AirPlayScreenHeader`: `bodySize u32 LE @0`, `opcode u8 @4`, sync-sample
(keyframe) flag `hdr[5] & 0x10`. **opcode 1 = VideoConfig — `body` is plaintext avcC/hvcC: do not
decrypt it and do not advance the counter.** opcode 0 = VideoFrame: nonce = `[0,0,0,0]‖seq_le64`
**taken from the wire `seq` field**, AAD = the whole 128-B `hdr`, ciphertext‖tag = `body`. `seq` is the
iPhone's own per-VideoFrame nonce counter and advances on opcode-0 messages only; the box advances it
even for cluster frames it gates away, so **a host that increments a local counter instead of reading
`seq` desyncs permanently at the first gap and fails every Poly1305 tag thereafter** (the `ocbm-host`
#678 fix). A `seq` jump is the loss signal — resync the counter from it and request a keyframe.
`SEAM_MAGIC` is these lanes' only resync marker: on a torn message, scan forward for it and re-align on
`magic − 4`.

**Audio lanes** (seam framing v2). Envelope `[u32 BE len][SEAM_MAGIC "SEAV" 4B][marker u8]…` — the
SAME envelope as the video lanes, `len` counting the magic — with every message scid-tagged so
concurrent streams sharing the voice sink (telephony + alert) cannot clobber each other:
- `SEAM_KEY    0x00` — `[len=45]["SEAV"][0x00][key.output 32B][scid u64 LE]`  *(scid TRAILS the key here)*
- `SEAM_PKT    0x01` — `[len=13+pkt]["SEAV"][0x01][scid u64 LE][raw encrypted RTP datagram]`
- `SEAM_FORMAT 0x02` — `[len=21]["SEAV"][0x02][scid u64 LE][codec u8][rate u32 LE][ch u8][bits u8][audio_type u8]`
  (codec 0 PCM · 1 AAC-LC · 2 AAC-ELD · 3 OPUS · 4 mSBC; `bits` 16 for PCM else 0; audio_type 0 media ·
  1 telephony · 2 speechRecognition · 3 alert · 4 default · 5 compatibility — the PCM media
  fallback, split out of 4 so a consumer cannot misroute it by format alone; same values as the
  `:9003` `atype` table above)
- `SEAM_PKT_PLAIN 0x03` — `[len=13+pcm]["SEAV"][0x03][scid u64 LE][payload]` — an **UNENCRYPTED**
  access unit: no RTP header, no tag, no nonce, and **no `SEAM_KEY` is ever sent for that scid**.
  Added 2026-09-03 for the Android Auto **telephony** lane: the call's audio reaches the box over
  Bluetooth HFP/SCO (CVSD, 8 kHz mono S16LE), where there is no AirPlay stream to encrypt it with, so
  the box forwards 320 B (= 160 samples = 20 ms) per frame verbatim on `CH_ALT_AUDIO` after a
  `SEAM_FORMAT` of `codec 0 PCM, rate 8000, ch 1, bits 16, audio_type 1`. The payload is
  **little-endian** (Android-native), unlike the big-endian PCM inside the CarPlay RTP — a host that
  byte-swaps it plays full-scale white noise. A `SEAM_PKT_PLAIN` arriving before its `SEAM_FORMAT` is
  dropped (the rate is unknowable) and counted; the host must not run the RFC 2198 demux over it.
  Uplink for the same call is the existing mic path: the box's `CT_UPLINK` gate asks for
  `uplink on 8000 1` and the host answers with S16LE 8 kHz mono on `CH_MIC` in 20 ms (320 B) frames.
  **Wideband (codec 4 mSBC), added 2026-09-04 behind the box lever `CARPLAY_HFP_WBS` / `/tmp/hfp_wbs`:**
  when the AG negotiates mSBC the controller stops decoding, and the box forwards each transparent
  eSCO read VERBATIM as one `SEAM_PKT_PLAIN` — no 320 B aggregation, H2 headers untouched — under a
  `SEAM_FORMAT` of `codec 4, rate 16000, ch 1, bits 16, audio_type 1`, where `rate`/`bits` describe
  the DECODED audio and the payload is a bitstream. The host resynchronises on the H2 header
  (`0x01` then `0x08`/`0x38`/`0xC8`/`0xF8`), never on message length, and must drop the stream rather
  than play it if it cannot decode mSBC. The gate then reads `uplink on 16000 1 msbc` → `CT_UPLINK`
  codec 4, and the host returns whole 60 B packets (H2 + 57 B frame + 1 pad) on `CH_MIC`; the box
  writes them to the SCO socket unmodified and SKIPS a write it has no whole packet for.
  **Box producer:** `crates/vendor/wireless/src/sco_audio.rs` — it connects to ocbmd's voice seam
  `:9003` when the phone opens SCO and uses a FIXED scid `0x4846_5053_434F_0001` (ASCII `HFPSCO` +
  an ordinal), because there is exactly one SCO channel at a time; a scid in a host log therefore
  names its own origin. The uplink half has no ocbmd change at all: ocbmd's `CH_MIC` relay already
  connects to `127.0.0.1:9112`, and during an Android Auto session — when airplayd is not running —
  `carplay-wireless` listens there itself and speaks airplayd's protocol verbatim.

The datagram is the iPhone's packet verbatim: `[12B RTP hdr][ciphertext][16B tag][8B nonce]`. Nonce =
`[0,0,0,0]‖pkt[len-8..]`, AAD = `pkt[4..12]` (ts‖ssrc), ciphertext‖tag = `pkt[12..len-8]`. **This lane
has no sequence and no counter** — the nonce rides every packet.

**CORRECTED 2026-09-03 — the audio lanes DO carry `SEAM_MAGIC` now.** Until this date they had no
magic and a torn seam could only re-frame by length plausibility, which did not survive a re-SETUP:
ocbmd replaces a seam producer without draining the old one, so the host's reassembly for that channel
could still hold a partial message when the new producer's `SEAM_KEY` landed mid-message and desynced
the lane for the rest of the session. Device-proven: 18 bogus `received audio key (scid=…)` lines, an
`audio format … 1469658167Hz 232ch`, and no media audio on 3 of 4 streams. Fixed by the magic above
(`receiver::session::seam_audio_key_msg` / `seam_audio_format_msg`, the `SEAM_PKT` write in the audio
thread) together with `F_NEW_SOURCE` (see the flag catalog above) telling the host to drop the stale
partial before appending — a host must accept a **pre-magic box build** by falling back to the legacy
`[u32 BE len][marker]` parse when the magic is absent on the first message of a seam buffer (macOS
`OCBMAVDecrypt.nextAudioMessage`, Android `AudioSeam.drain`).

Multibyte fields: seam length prefixes are **big-endian**; every other seam field (`seq`, `scid`,
`rate`, the screen header's `bodySize`) is **little-endian**.

The key handoff is **not one-shot**: the video lanes re-hand it on every seam (re)connect, the audio
lanes whenever a sink write fails (re-sending `SEAM_FORMAT` with it), so a late-joining or reconnecting
host consumer is never left keyless or format-blind. Note the trigger is the **box-local seam socket**,
not the OCBM channel — ocbmd holds its accepted seam across a host detach, so a host re-attaching to a
still-running session depends on the presence teardown/re-arm, not on a fresh hand. A Rust debug
receiver (`ocbm-host avdec`) validated the model live — hundreds of video frames + thousands of audio
packets, **0 auth failures** — on CH_VIDEO and both audio lanes; CH_ALT_VIDEO is exercised by the macOS
host only.

**All of the above is the `OCBM_FWD_ENC` (default-on) wire.** With `OCBM_FWD_ENC=0` the box decrypts
on-box and none of it applies: the video seam carries `[u32 BE len][Annex-B]` with no magic, marker, key
or seq, and the audio seams carry the legacy `:9002` bare-ADTS / `:9003` 11-byte-tag framing described
above. The two are wire-incompatible with no discriminator on the wire.
The relayed-RTSP channel now exists — **`0x0041` CH_RTSP**, below; media-port coordination stays an
open design item (see `../ops/04_OPEN_ITEMS.md` §Open design items).

### RTSP channel (`0x0041`) — app-driven SETUP relay (plan P1)

The box runs its unmodified `AvSession` first (**pre-bind + oracle**: every side effect happens
box-side and the local response doubles as the report), then relays the decrypted request *plus that
local response* to the host, which authors the response the phone actually sees. Any relay failure
(timeout 3 s / `RS_ERR` / non-200 / seam death) falls back to the in-hand local response — sticky for
the rest of that phone connection. Wireless was flipped 2026-08-10 — the relay now runs on both
transports; the host preserves the box-only `iAPChannel`/`sessionManagement` tokens that only the
wireless arm emits.

ocbmd chunks the `:9106` seam bytes into ≤64 KiB OCBM frames both ways; **all message framing is
endpoint-to-endpoint** (airplayd ↔ host app): `[u32 BE 0x52545350 "RTSP"][u32 BE len][msg]`, receiver
resyncs by magic scan. Messages share the header `[op u8][conn u32 LE][cseq u32 LE]`; `conn` is
monotonic per airplayd process (hijack ⇒ new conn, and the serve FIFO guarantees `RS_CLOSE(old)`
precedes `RS_OPEN(new)`), the host drops messages for a non-current conn. Constants are authoritative
in `receiver::relay` and mirrored in `ocbm-proto`/Swift (the META_* pattern).

| Op | Message | Direction | Payload after the common header |
|---|---|---|---|
| `0x01` | RS_OPEN | box→host | `[ver=1][flags b0=wireless][cfg_crc u32 LE][ctx_len u32 LE][ctx bplist {peer, displayWidth, displayHeight}]` — `cfg_crc` = crc32 of the YAML this connection's `/info` was built from (0 = built-in default); host compares vs what it pushed at SUBSCRIBE |
| `0x02` | RS_REQ | box→host | `[route u8][flags b0=NOTIFY][local_len u32 LE][local-resp bplist][req bplist]` — routes: 1 = SETUP, 2 = RECORD, 3 = TEARDOWN (always NOTIFY); 4–7 reserved. Phase 1 vs 2 SETUP is distinguished by the `streams` key, same as the box |
| `0x03` | RS_RESP | host→box | `[status u16 LE][response body]` — non-200 = host-reject → local fallback (v1) |
| `0x04` | RS_CLOSE | box→host | `[reason u8]` — 0 eof · 1 hijack · 2 error · 3 reset |
| `0x05` | RS_ERR | host→box | `[code u8]` — box falls back to its local response |

Harness: `ocbm-host setup-relay [vid pid] [secs] <cfg.yaml> [--author] [--mute-once]`. Default is
Stage-0 transparency — echoes every local response verbatim. `--author` is Stage 1 (plan P2): author
each response host-side and diff it against the box's local one. `--mute-once` deliberately DROPS the
first `RS_REQ` (authors and sends nothing) to exercise the box's 3 s timeout → sticky-local fallback.
The YAML must set `accessoryConfig.appDrivenSetup: true`, see `tools/setup_relay_cfg.yaml`.

Because the A/V channels are the **live-UI** path, ocbmd carries them on dedicated **per-stream**
queues (video / alt-video / audio) with **backpressure, not drop**: the poll loop only pulls a
stream's next seam chunk once that stream's queue has drained, so a slow USB/host propagates back
through the seam → airplayd → the iPhone's TCP screen socket. (That iOS then adapts its encode rate is
the design expectation, not a measured result.) **Video only:** `CH_MEDIA_AUDIO` and `CH_ALT_AUDIO`
share one **ungated** queue, because their source is RTP over UDP and there is no transport flow control
to propagate — a long host stall grows that queue to its cap and then drops whole frames, so in practice
`av_dropped` is an audio counter. On the video lanes the cap is unreachable by construction and is an OOM
backstop, not a drop policy (see [`../carplay/02_SESSION_LIFECYCLE.md`](../carplay/02_SESSION_LIFECYCLE.md)).

An early draft of this doc carried a *decoded* stream taxonomy (VOICE_AUDIO / DUCK / METADATA,
payloads lifted from the `carplayd` IPC contract) under ids `0x0022` / `0x0031` / `0x0040`. Those ids
have since been assigned to **real wire channels** (CH_ALT_AUDIO / CH_MIC / CH_MGMT — see the table
above), so the taxonomy is dropped from this spec to avoid the collision trap. What the host app
produces internally after decrypt/decode is an app-side concern, not an OCBM wire format.

### LOG channel (`0x0042`) — the box's logs, live

**Why it exists.** Everything the box knows about a failure it writes to a file in `/tmp`, and until
this channel the only ways to read one were a debug UART, an NCM/SSH route the shipped head-unit link
does not have, or a `FILE_PULL` after the fact. "What was the box doing when it dropped us" was
answerable only in a lab.

**`/tmp/box.log` is the box's universal log**, and the one the box OWNS: every daemon and script whose
output nothing else parses appends to it with `O_APPEND` (the run scripts redirect stdout/stderr there,
ocbmd's own included), and lines carry their own `[ocbmd]` / `[airplayd]` / `[sup]` prefixes. `/tmp` is
tmpfs on a 123 MB no-swap box, so it is a bounded **staging area, never storage**: the tailer
`ftruncate`s it back to 0 at the cap. The file is small by construction, which is why "stream from
offset 0" IS the backfill — everything since boot, with no separate dump opcode.

**Not everything could be funnelled into it.** `session_supervisor.sh` and `projection_up.sh` PARSE
the per-daemon logs as IPC — the pair-verify `grep -q`, the `tail -1` stall checks, `bound_logs`' own
reap list — so those files keep their own identity and lifecycle. The tailer follows them **tail-only**
and never writes to them; only source 0 is rotated. Sources:

| `source` | Name | Path | Policy |
|---|---|---|---|
| `0` | `box` | `/tmp/box.log` | **staged** — streamed, then `ftruncate`d at `cap_kb` |
| `1` | `airplayd` | `/tmp/airplayd.log` | tail-only |
| `2` | `airplayd_wl` | `/tmp/airplayd_wl.log` | tail-only |
| `3` | `iap2d` | `/tmp/iap2d.log` | tail-only |
| `4` | `aa-bridge` | `/tmp/aa-bridge.log` | tail-only |
| `5` | `rx-connect` | `/tmp/rx-connect.log` | tail-only |
| `6` | `bt` | `/tmp/bt.log` | tail-only |
| `7` | `radio_ap_dhcp` | `/tmp/radio_ap_dhcp.log` | tail-only |
| `8` | `radio_bt_attach` | `/tmp/radio_bt_attach.log` | tail-only |
| `9` | `rx-connect_wl` | `/tmp/rx-connect_wl.log` | tail-only |
| `10` | `wl` | `/tmp/wl.log` | tail-only (carplay-wireless stdout) |
| `255` | `internal` | — | the tailer itself: rotation / restart notes and drop reports |

An **unknown source id is a display concern, never a reason to drop the entry** — a newer box may
follow sources a client has never heard of. Ids are fixed by agreement with the host apps; renumbering
would silently relabel every line a shipped client renders.

**Entry**, back-to-back, packed to at most `LOG_MAX_FRAME` = 4096 B of payload per frame:

```
off 0:  source  u8         ; the table above
off 1:  flags   u8         ; bit0 LOG_F_DROPPED, bit1 LOG_F_TRUNCATED, bit2 LOG_F_BACKFILL
off 2:  seq     u16 LE     ; per-channel entry counter, wraps; advisory ordering only
off 4:  unix_ms u64 LE     ; wall clock — bogus until CT_SETTIME lands (no RTC battery)
off 12: len     u16 LE     ; <= LOG_MAX_LINE (1024)
off 14: text    [len]      ; UTF-8 (lossy-converted box-side), NO trailing newline
```

- `LOG_F_TRUNCATED` (`0x02`): the source line was longer than 1024 B and `text` is its prefix.
- `LOG_F_DROPPED` (`0x01`): this entry is a **drop report**, not a line — `len` is exactly 4 and
  `text` is a `u32 LE` count of lines lost to the box's queue cap since the previous report, for the
  `source` named in the entry. Reports are **prepended** to the next frame, so a host renders the gap
  where it happened. A decoder must reject a `LOG_F_DROPPED` entry whose `len` is not 4, a `len` above
  1024, and a `len` past the end of the payload: entries are self-delimiting, so one tolerated bad
  length walks the reader off the end of every entry behind it.
- `LOG_F_BACKFILL` (`0x04`): this line was already on disk when the tailer (re)opened its source at
  offset 0 — the enable-time backfill, or any restart forced by an in-place truncation or a
  replaced/reaped file (§ above). Never set on a line the tailer read after that point. **Fixes a
  device-proven defect:** before this flag, a reconnect re-streamed the same history with a FRESH
  `unix_ms` each time (the same line looked like 5 different live events across 5 app launches),
  and a host had no way to tell backfill from live at all. A reader should render `LOG_F_BACKFILL`
  lines distinctly (dimmed / filterable) rather than as new activity.

**`unix_ms` write-time vs. read-time.** By default `unix_ms` is stamped when the tailer READS the
line, which collapses an entire burst written between two ticks onto the one millisecond it was
read at. A writer that knows its own wall-clock time may instead prefix the line itself with
`@<unix_ms> ` — ASCII decimal digits, then exactly one space, before the rest of the text (e.g.
`@1758382920123 [iap2] numeric-comparison code = 874736`). The tailer recognizes this convention on
any source, parses the digits as the entry's `unix_ms`, and strips the prefix from `text` before
encoding the entry; a line that does not open with it keeps the read-time stamp. Central Rust log
helpers that funnel a daemon's own log lines (`iap2d`, `bt-common`'s `sdp_server`/`ssp_agent`,
`carplay-wireless`'s `main`/`bt_driver`/`control`/`sdp_client`/`reconnect`/`arbiter_client`,
`receiver`'s `iap_tunnel`) emit this prefix on every line; ocbmd's own `eprintln!`s into `box.log`
and the shell-script writers (`session_supervisor.sh`, `ocbm_boot.sh`, `radio_hal.sh` et al.,
`aa-bridge`/`rx-connect`'s ad hoc `eprintln!`s) do not, and stay read-time-stamped — deliberately:
this is a cheap `SystemTime::now()` in a handful of central Rust helpers, not a per-line shell pipe
in the supervisor, which is CPU-bound.

**Box-side discipline.** The tailer runs on ocbmd's single dispatch thread on the same `Instant`-gated
tick as the `/tmp/bt_phase` mirror (~250 ms), never blocks (`O_NONBLOCK`), and reads at most **8 KB per
tick across all sources** — round-robin, so a chatty file cannot starve the rest, with the leftover
budget handed to whoever still has data so one active source still gets the whole 8 KB. A source that
**shrinks** (in-place `bound_logs` rewrite) or is **replaced** (reaped and recreated — detected by
inode, one path `stat` per tick, rotating) restarts at offset 0 with a `source 255` note saying why; an
absent file is simply polled for. `CH_LOG` drains **below `CH_CONSOLE` and above the bulk queue**: a
diagnostic must never delay the control plane, A/V, or an interactive rescue console, but it must not
sit behind a 32 MiB file pull either. The whole log path is capped at 64 KiB of RAM; over it the
**oldest** pending entries are dropped and counted, because the newest lines are the ones describing
whatever is going wrong now.

**Cap enforcement runs whether or not a host is streaming** — an idle box with no host is where this
file spends most of its life growing. Disabled and over cap, the box `ftruncate`s `/tmp/box.log` to 0
and appends one `[log] rotated <N> bytes (not streamed)` marker with `O_APPEND`. (This is why ocbmd's
poll no longer blocks indefinitely when fully idle: it wakes at 2 s to run that one `stat`.)

### FILE channel (`0x0011`) — verified binary deploy

The box rootfs is a tiny jffs2 and the shipped head-unit link is OCBM-only (no NCM/SSH), so
deploying a rebuilt daemon otherwise means base64-over-UART, which drops bytes without hardware
flow control and silently corrupts. The FILE channel makes a binary deploy ride the reliable
accessory pipe instead. The **box** advertises `CAP_FILE` (`0x10`) in its `HELLO_ACK` caps.

One transfer is active at a time. Payload = `[type u8][…]`:

| Type | Message | Direction | Payload after the type byte |
|---|---|---|---|
| `0x01` | `FILE_OPEN` | host→box | `[mode u32 LE][path bytes]` — box creates `<path>.ocbm.part` |
| `0x02` | `FILE_DATA` | host→box (and box→host during a pull) | `[chunk bytes]` — box appends, updates a running CRC-32 |
| `0x03` | `FILE_CLOSE` | host→box | `[crc32 u32 LE][size u32 LE]` — box verifies, `chmod`s, renames |
| `0x04` | `FILE_ACK` | box→host | `[status u8][crc32 u32 LE][size u32 LE]` |
| `0x05` | `FILE_PULL` | host→box | `[path bytes]` — box→host retrieval, the mirror of push (below) |

**Pull flow:** the box validates the path like `FILE_OPEN` (absolute, no `..`) plus regular-file-only
and ≤ 32 MiB, then streams the file back as box→host `FILE_DATA` sub-frames — paced to the host's
drain rate, with a per-chunk stall deadline so a dead host can't wedge the dispatch loop — and
terminates with a `FILE_ACK` carrying (`FILE_OK`, crc32, size), which the host reassembles against
and verifies end-to-end; a bad path / open failure gets a single `FILE_ACK(FILE_ERR_OPEN|FILE_ERR_NOFILE)`.

`FILE_ACK.status`: `0` ok · `1` open failed · `2` crc/size mismatch · `3` no open file ·
`4` write/read/rename failed. The box acks `FILE_OPEN` and `FILE_CLOSE`; `FILE_DATA` is acked **only on
error** (silent on success, so the pipe stays full). On `FILE_CLOSE` the box compares the
end-to-end CRC-32 (zlib/IEEE, matches `python3 -c "import zlib;…"`) and byte count, `fchmod`s to the
requested `mode`, then **atomically renames** the temp onto the final path — a mismatched or aborted
transfer leaves neither a half-written binary nor a stale non-executable one. `mode` carries the
exec bits (e.g. `0o755`), which fixes the long-standing "deploy dropped the +x bit → unbootable
rcS" gotcha. This is the incremental, always-available cousin of the `FIRMWARE_UPDATE` mode below.

## Self-describing streams (how features stay data, not protocol)

**There is no `STREAM_OPEN`/`STREAM_CLOSE`, on CTRL or anywhere else.** An earlier draft of this
document sketched a `STREAM_OPEN { streamId, streamType, codec, params (TLV) }` on CH_CTRL. It was
never implemented: no such opcode exists in `crates/ocbm-proto/src/lib.rs` (the `CT_*` space,
`0x01`-`0x1B`, is handshake, session lifecycle, box->host status and the log-stream arm only), and nothing in the
workspace emits or parses one. The sketch is recorded here as history so it is not re-derived;
**do not implement against it.**

The *goal* it expressed - the envelope never learns about features; a stream's role, codec and
parameters travel as data - is met today by four real mechanisms:

1. **Channel id = stream role**, fixed at design time rather than assigned at runtime.
   `CH_VIDEO 0x0020` main screen, `CH_ALT_VIDEO 0x0024` cluster, `CH_MEDIA_AUDIO 0x0021`,
   `CH_ALT_AUDIO 0x0022` voice sink. The box binds AirPlay stream types to lanes statically
   (`receiver::session`: type 110 -> seam `:9001`, type 111 -> `:9005`; audio routes by
   `audioType == "media"` -> `:9002`, else `:9003`), and ocbmd maps seam port -> channel from a fixed
   table. A lane's identity is structural, so a receiver needs no stream registry.

2. **Audio: `SEAM_FORMAT` is the per-stream descriptor** - `[0x02][scid u64 LE][codec u8]
   [rate u32 LE][ch u8][bits u8][audio_type u8]`, on the media lane itself (see §Media transport for
   the byte-exact framing). Codec `0 PCM / 1 AAC-LC / 2 AAC-ELD / 3 OPUS / 4 mSBC`; `audio_type`
   `0 media / 1 telephony / 2 speechRecognition / 3 alert / 4 default / 5 compatibility`. Every
   `SEAM_KEY`/`SEAM_PKT` is scid-tagged too, so **N concurrent audio streams share one channel** and
   a host keeps per-scid key/format/decoder tables. New codecs are new enum values; an unknown value
   is logged and its AUs dropped, never fatal. This *is* the "audio descriptor" the sketch wanted,
   just on the data lane rather than CTRL.

3. **Video: the codec is in-band and implicit.** There is no codec field on any OCBM video message.
   Screen-header **opcode 1 = VideoConfig** carries a plaintext `avcC` (H.264) or an ISO
   `hvc1`/`hev1` VisualSampleEntry wrapping `hvcC` (HEVC); under `OCBM_FWD_ENC` the box forwards it
   untouched and **the host sniffs which it is** and (re)configures its decoder. Switching codec
   mid-session therefore needs no protocol change - but equally, nothing on the wire ever *names*
   the codec.

4. **`CH_RTSP 0x0041` is the real dynamic stream-open channel.** With `appDrivenSetup`, the phone's
   own decrypted SETUP request - its `streams[]` array of `{type, streamConnectionID, audioFormat,
   input, dataPort}` - is relayed box->host as `RS_REQ`, and the host authors the per-stream response
   the phone sees (`RS_RESP`). That is a self-describing, dynamic, per-stream open/close negotiated
   in Apple's own vocabulary, which is strictly better than re-encoding it as our own TLV.

**What is genuinely not supported (open item).** Video lanes are **fixed and capped at two** - one
main, one alt/cluster. The box dispatches only AirPlay stream types 110 and 111, each to its own
seam, queue and channel; `videoStreamsConfig.altVideoStreams[]` is parsed as an array but only its
first entry is ever used, and `/info` advertises at most two `displays[]`. The video seam's key
message does carry an `scid`, but the host discards it and keys nothing by it. A third video stream,
or two multiplexed on one video channel, would need new channel ids (or adopting the audio lanes'
scid tagging on the video lanes) - **not** a new CTRL message. Audio has no such limit.

Consequently:

- **HEVC** is not a codec enum on OCBM. It is `accessoryConfig.enablesHEVC` in the pushed YAML ->
  the receiver's HEVC lever -> `hevcInfo = {}` in `/info` + `"hevc"` in the SETUP-response
  `enabledFeatures`, after which iOS's choice arrives as the opcode-1 config record the host sniffs.
- **Alt/cluster screen (type-111)** is not "a second VIDEO stream with `streamType=altScreen`". It is
  a second `displays[]` entry (own uuid/dims/viewAreas + `altScreenURLs`) gated on the YAML's
  `altVideoStreams[]`, echoed as `"altScreen"` in `enabledFeatures`, carried on its own channel
  `CH_ALT_VIDEO`, and forward-gated default-OFF box-side until the host sends
  `CMD_NAV_START/CARD/APP`. It is addressed by that display's **uuid**, never by a stream id.
- **`viewAreas`/`safeArea` are not stream params.** They are `/info` `displays[].viewAreas`
  structures built from the pushed YAML and negotiated with iOS (honoured only when `"viewAreas"` is
  echoed in `enabledFeatures`) *before any stream exists*. They could not ride a host<->box stream
  open even in principle.
- **Wireless / buffered audio** is a `SEAM_FORMAT` codec byte plus the YAML `audio` block, which
  chooses which formats `/info` advertises.
  > **Format lists in this document are OCBM extensibility illustrations, NOT CarPlay capability
  > claims.** **CarPlay negotiates none of DDP, QAAC or Atmos** - `kAirPlayAudioFormat_*` tops out at
  > stereo AAC-LC 48 kHz, with no channel count above 2 and no object-based entry in any SDK
  > revision. See [`../carplay/06_AV_PIPELINE.md`](../carplay/06_AV_PIPELINE.md).

## Extensibility rules (the "no rewrite, ever" contract)

1. **Frozen envelope.** The 16-byte header never changes; `length` makes unknown content skippable.
2. **Broad channels**, never codec-specific — honoured for the transport-shaped lanes, but
   `CH_ALT_VIDEO` / `CH_ALT_AUDIO` / `CH_MIC` are **role**-specific by deliberate decision, because a
   shared lane would have forced host-side demux and a second decoder off one buffer.
3. **Capability negotiation** mirroring CarPlay's `features:[]` + `audioFormats` bitmask; operate on
   the intersection; graceful fallback.
4. **Self-describing streams — as data on the stream's own lane, not as a CTRL stream-open.** Audio
   carries a per-stream `SEAM_FORMAT` descriptor inline on the media channel, and every audio message
   is scid-tagged so one channel carries N concurrent streams. Video carries its codec in-band as the
   plaintext opcode-1 avcC/hvcC config the host sniffs. Per-stream *negotiation* is relayed verbatim
   on `CH_RTSP` (`RS_REQ`/`RS_RESP`) in Apple's own SETUP vocabulary. A stream's *role* is its channel
   id, fixed at design time — which is why video is capped at `CH_VIDEO`/`CH_ALT_VIDEO` and a third
   video stream would need a new channel id, not a new message. **There is no `STREAM_OPEN`.**
5. **TLV / keyed metadata + control**; hard rule: **skip unknown TLVs/keys/streamTypes/codecs**,
   never fatal.
6. **Reserved + experimental ranges** — add channels/enums without collision or a version bump.
7. **`IP` tunnel as the universal escape hatch** — any un-modeled CarPlay feature works day one as
   opaque bytes; later *promote* it to a typed channel by emitting a decoded stream instead of
   tunneling, **with no protocol change** (progressive enhancement).

The only thing that could force a real rewrite is changing the envelope itself — which
capability-versioning lets you avoid even then.
