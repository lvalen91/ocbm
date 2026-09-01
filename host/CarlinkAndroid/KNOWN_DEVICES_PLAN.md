# Known devices + preferred phone — implementation plan

Consolidated from a 12-agent design pass, 2026-08-16. Every claim is traceable to code or to a
licensed Apple source; where the agents disagreed, the disagreement and its resolution are stated
rather than smoothed over. Where something could not be determined, it is listed as unknown rather
than guessed.

---

## 1. The requirement

> "Known device list should be the history of devices until/unless cleared or app storage cleared in
> Android. It shouldn't wait to populate the list until a device connects. It should list the 'Known'
> devices. And the first device or device selected or last-connected device should be the device the
> adapter attempts to connect to. If the adapter/box 'vehicle' reached out first, it should be
> recognized by iOS to then attempt CarPlay pairing/handshake."

Four separable pieces: **persistent history**, **a preferred device**, **the box acting on it**, and
**iOS accepting the box-initiated connect**. The fourth is already done (§2.1).

---

## 2. Established facts

### 2.1 The accessory-initiated path already works — device-proven

`docs/wireless/01_BT_AND_RADIO.md` records it RESOLVED. The implemented
sequence, all of it already in the tree:

| # | Step | Layer | Where |
|---|---|---|---|
| 1 | Page the phone implicitly by L2CAP-connecting to its SDP PSM 0x0001 | baseband/L2CAP | `sdp_client.rs:1-12` |
| 2 | SDP query for the **phone-side** iAP2 UUID `02030302-1d19-415f-86f2-22a2106a0a77` → RFCOMM channel (observed: 1) | SDP | `sdp_client.rs:26-33` |
| 3 | Accessory becomes RFCOMM **client**, connects OUT (link-key auth happens here) | RFCOMM | `rfcomm::connect_to` |
| 4 | iAP2 link SYN/SYN-ACK — a fresh link, never a resume | iAP2 link | docs/wireless/01_BT_AND_RADIO.md:58-62 |
| 5 | MFi auth via the genuine coprocessor → 0xAA05 Authenticated | iAP2 | `bt_driver.rs:44-51` |
| 6 | 0x1D01 Identify → 0x1D02 Accepted | iAP2 | docs/wireless/01_BT_AND_RADIO.md:63-64 |
| 7 | **iOS drives**: 0x5702 asks for WiFi config → accessory answers 0x5703 with live AP creds | iAP2 | `wifi_handoff.rs:9-24` |
| 8 | 0x4E0E DeviceTransportIdentifier (phone's **Wi-Fi MAC** + device-id — *corrected 2026-08-16, this said "BT MAC"*; per `bt_driver.rs` it is the one place the phone's true Wi-Fi MAC is exposed, ARP showing only its private MAC) | iAP2 | `bt_driver.rs`, the 0x4E0E arm |
| 9 | Phone joins the AP; airplayd + rx-connect come up; accessory connects OUT for CarPlay Control | WiFi/mDNS | `av.rs:1-14` |
| 10 | AirPlay pair-verify, SETUP, A/V | RTSP | R14G17 IntegrationGuide |

iOS's own `carkitd` instruments the same stage order (`reconnectionBTTime → …iAPTime →
…WifiBasicAssocTime → …AirPlayTime`).

**iOS never self-initiates and will not escalate a bare inbound connection.** Device-proven
eliminations (docs/wireless/01_BT_AND_RADIO.md:96-109, marked "do NOT retry"): a bare HCI page is *seen* but nothing starts;
page-plus-held-L2CAP is accepted and then iOS sits idle for 12 s. Escalation happens only when an
iAP2 link comes up on RFCOMM and the handshake completes. "The accessory drives everything."

### 2.2 Apple puts the preference in the head unit

No preferred-device mechanism exists in any Apple source examined. WWDC 2017-717 (recorded in
`docs/20` §2.4-2.5) prescribes: the **head unit** stores the device as last-connected/preferred,
**first-to-connect wins, never preempt a running session**. This is accessory-side policy by design —
we are not working around the protocol.

iOS keeps CarPlay authorization keyed on the **MFi cert serial**
(`isCarPlayPairedWithCertSerial:`), separately from the BT bond. "Bonded" and "CarPlay-authorized"
are genuinely different states.

User-facing gates found in Apple's binaries: the "Use wireless CarPlay" consent card (with a
"Don't use" option), the "allow while locked" card, per-device Forget in CarAccessoryFramework, and a
**WiFi-off interlock** — if the phone's WiFi is off when a known car connects, iOS raises an
enable-WiFi alert rather than proceeding. That last one is a plausible "my preferred phone won't
connect" report that is not our bug.

### 2.3 The box cannot supply the list

`/etc/carplay/bt_link_keys` (`ssp_agent.rs:115-116`, path composed by `link_key_store()` at `:127-131`)
is a flat concatenation of 25-byte records, verbatim
the Linux mgmt Load-Link-Keys layout:

```
[bdaddr 6][addr_type 1][key_type 1][value 16][pin_length 1]   = 25 bytes
```

All 25 bytes are accounted for: **no name, no timestamp, no device class**. `MGMT_INFO.devices` is a
bare MAC array built from it (`bonded_macs()`, `ocbmd/src/main.rs:777-790`; emitted at `:2094-2105`). The AirPlay peer store
(`/etc/carplay_peers.bin`) is likewise `[id_len][id][32-byte LTPK]` — no metadata either.

**Therefore "last connected" must be tracked by the app.** This matches docs/carplay/04_CAPABILITIES_AND_CONFIG.md's app-owns-state
doctrine and the requirement's own framing.

### 2.4 Today's reconnect order is arbitrary, and effectively backwards

`persist_link_key` (`ssp_agent.rs:159-172`) rewrites the file as *everything except this bdaddr*,
then appends. So file order = **pairing recency, oldest first**, and a re-pair moves that device to
the end. Ordinary reconnects never touch the file.

`reconnect.rs:127,144` iterates exactly that order. So the box currently pages your **oldest-paired**
phone first, and store order carries no information about recent use.

### 2.5 Identity is already available

`CT_PHONE_IDENT` (0x18, landed 2026-08-15) carries `{name, deviceID, model, osName, osVersion}` from
the phone's own AirPlay phase-1 SETUP plist. `deviceID` is the BR/EDR MAC — the join key against
`MGMT_INFO.devices`. Verified on hardware: `name=<owner> iPhone`, `deviceID=64:31:35:8c:29:69`.

### 2.6 The old firmware really did have this

Agent 7 found it, with wire shapes:

- `AutoConnect_By_BluetoothAddress` — type **0x11**, H→A, payload a bare 17-byte ASCII MAC
  (`carlink_native_personal/.../MessageSerializer.kt:142-146`). Firmware side binary-verified: the
  auto-connect path requires `NeedAutoConnect=1`, reads the stored MAC, initiates the connection.
- `ForgetBluetoothAddr` — type **0x22**, same payload; moves the device from `DevList` to
  `DeletedDevList`.
- `BluetoothPairedList` — type **0x12**, A→H, concatenated `MAC+Name` (sometimes null-separated,
  sometimes not — the old app recovered it with a MAC regex).
- Commands `SupportAutoConnect=1001`, `StartAutoConnect=1002`, `GetBluetoothOnlineList=1013`.

Storage was **on the adapter** (`DevList` in `/etc/riddle.conf`), with fields `id`/`name`/`type`/
`time`/`rfcomm`. The real `DevList` from this project's own backed-up unit still contains
`64:31:35:8C:29:69 "iPhone"`.

Stock policy was `LastConnectedDevice` — a single MAC the firmware overwrote at every session start.
**No user-designatable primary existed**; the old app bolted targeting on via 0x11.

**The recorded stock bug is a design constraint for us:** ForgetBluetoothAddr removed the device from
`DevList` but did **not** clear `LastConnectedDevice`, so a forgotten phone remained the
boot-reconnect target (`revisions.txt:274`). The list and the preferred pointer must move as one unit.

---

## 3. Architecture decisions

### D1. Wire: `MGMT_SET_PREFERRED = 0x06` on CH_MGMT — not the pushed YAML

Agents 4 and 8 argued for a YAML field. **Rejected** on agent 3's evidence:

1. **It would tear down the live session.** A mid-session re-SUBSCRIBE with changed cfg deliberately
   forces the silent re-arm / session rebuild (the `if (reusing && cfg_changed) || replaced` branch of
   ocbmd's `CT_SUBSCRIBE` arm, `ocbmd/src/main.rs:2265-2271`). Changing a preference
   must not do that. Direct precedent: `CT_RADIO` was made a control message *instead of* a config
   field for exactly this reason, and says so in its own comment.
2. **Wrong consumer.** `ocbmd` treats the pushed cfg as opaque bytes and never parses it
   (`main.rs:2246,2256-2258` — a byte compare and a verbatim `write_cfg_file`); `carplay-wireless`
   never reads `/tmp/carplay_cfg.yaml` at all. Either way it needs new parsing machinery.
3. **No ACK path.**

Meanwhile the object being manipulated — a bonded MAC — already lives entirely on CH_MGMT:
`MGMT_FORGET_DEVICE` takes the identical payload, `MGMT_ACK` exists, and `MGMT_INFO.devices` is where
the UI already looks.

### D2. The preference is EPHEMERAL — `/tmp/preferred_phone`, re-asserted every session

Agent 11 proposed `/etc` so it survives reboot. **Rejected** on a functional argument (agent 10):

- `carplay-wireless` runs **only** while the host app is present — the supervisor brings it up on the
  `/tmp/host_present` 0→1 edge and tears it down on 1→0 (`tools/session_supervisor.sh:786-795`). There is
  **no app-less window** in which a persisted preference could ever be consulted.
- It would be the project's first rootfs write for UI state, against docs/carplay/02_SESSION_LIFECYCLE.md's "session config is
  ephemeral, always" and docs/carplay/04_CAPABILITIES_AND_CONFIG.md's app-driven doctrine. The only current `/etc` writers are
  pairing-class (`bt_link_keys`, `carplay_peers.bin`), justified by the phone's own protocol needs.
- Agent 11's own §6 argues the same way from the other end: a persisted preference is precisely the
  state that outlives a rollback and resurrects months later.

**The app is the durable store**; it re-sends after every `CT_SUBSCRIBE`.

### D3. Ordering, never filtering — plus demotion

Unanimous, and agent 8 quantified the danger. A preferred phone that accepts the RFCOMM DLC but never
completes iAP2 (CarPlay disabled for this car, mid-call — the case contemplated at
`reconnect.rs:155-160`) claims `session_active` and runs `bt_driver::run`, which has a **120 s
pre-Identify budget** (`bt_driver.rs:90`). While held, the accept path **drops inbound connects**
("session already active (reconnect in progress) -- dropping inbound connect", `wireless/main.rs:119`).

So a second phone's user tapping the car in Settings is silently refused for up to two minutes,
repeatedly. Filtering would also mean a dangling preference idles reconnect permanently.

**Design:** move the preferred bond to the front, keep everyone else in the rotation, same round.
After **N=3** consecutive attempts against the preferred peer that reach no iAP2 milestone, demote it
to the back until the next successful session or a preference change.

### D4. Two things the implementation must NOT do

- **Must not call `request_wireless_restart()`.** Copying `MGMT_FORGET_DEVICE`'s shape is the obvious
  implementation and it is wrong: the supervisor's `wireless_down` SIGTERM/SIGKILLs carplay-wireless
  **and airplayd/rx-connect** and powers `hci0` down (the `setsid sh -c` block at
  `tools/session_supervisor.sh:653-667`, inside `wireless_down()` at `:620`) — so setting a
  preference would **kill the currently projecting phone**. There is no rate limit on MGMT verbs.
- **Must not touch `/tmp/host_present`.** `rearm_presence_silently()` (`ocbmd/src/main.rs:1272-1278`)
  manufactures exactly the edges the flap detector counts: `FLAP_N=5` within `FLAP_WINDOW=20 s`
  (`tools/session_supervisor.sh:39-40`) fires `escalate`, and the ladder is L1 `phone_reset` ×2 → L2
  ocbmd restart ×2 → **L3 reboot**, against a persistent budget in `/etc/ccpa_reboot_count`. That
  chain has been observed live (`tools/session_supervisor.sh:467-472`).

The apply path is a **data-only write** picked up on the next reconnect round — the property audit
Fix #22 established for the bond list (`reconnect.rs:122-127`).

### D5. Never extend `bt_link_keys`

The loader does `chunks_exact(25)`. An appended preference trailer would be silently truncated, or —
if it happened to be a 25-byte multiple — **parsed as a bogus bond and loaded into the BT
controller** by rolled-back code. It would also race `ssp_agent`'s unlocked read-modify-write
`persist_link_key`. Separate file, always.

### D6. Feature-gate on `CAP_KNOWN_DEV = 0x40`

An old box drops unknown MGMT verbs at `_ => {}` (`main.rs:2060`, end of `handle_mgmt`'s match) with
**no ACK at all**, leaving the
app to sit through its 5 s timeout (mac: 6 s). The `CT_HELLO_ACK` caps bitmask already advertises
feature availability — exactly how `CAP_MFI` works — and Android already parses it
(`OcbmClient.kt:359-362`). Do not overload the version byte.

### D7. `MGMT_INFO` gains `"preferred"` — and nothing else changes shape

`devices` must stay a flat `[String]`. The macOS `CCPAInfo` is a Codable whose fields are **all
non-optional `let`s** (`OCBMClient.swift:696-720`): JSONDecoder ignores *extra* keys, but a removed or
retyped key fails the whole decode → the mac CCPA tab shows "Failed to read adapter info". The new
Swift field must be `let preferred: String?`.

No box-side names or timestamps: the record has neither, the box has no RTC battery (its clock is
host-set via `CT_SETTIME`), and identity enrichment belongs host-side per docs/carplay/04_CAPABILITIES_AND_CONFIG.md.

### D8. Unbonded-but-well-formed MAC: accept and store

Agent 10 said reject (`status 1`) — a dangling pointer silently redirects reconnect forever. Agent 12
said accept (`status 0`) — the bond may have been cleared and will re-form.

**Resolved: accept.** Agent 8's per-round validation makes a dangling preference harmless (ordering
ignores a MAC absent from `bonded_addrs()`), and the app's history *deliberately* contains devices
that are not currently bonded — that is the feature. Rejecting would make the box disagree with the
app's own model. Malformed MACs are still rejected with `status 1`.

---

## 4. Detailed design

### 4.1 Protocol

```rust
// crates/ocbm-proto/src/lib.rs
pub const MGMT_SET_PREFERRED: u8 = 0x06; // [MGMT_SET_PREFERRED][ascii MAC "AA:BB:.."] -> the reconnect
                                         // loop pages this bond FIRST each round. PREFER, not ONLY:
                                         // the other bonds stay in the rotation. Empty payload =
                                         // clear. EPHEMERAL (docs/carplay/02_SESSION_LIFECYCLE.md): held per session, cleared on
                                         // go_idle/startup, never persisted; the app re-asserts after
                                         // every SUBSCRIBE. ACKed via MGMT_ACK; the live value is
                                         // reported as MGMT_INFO's "preferred".
pub const CAP_KNOWN_DEV: u32 = 0x0000_0040; // box honours MGMT_SET_PREFERRED + reports "preferred"
```

Opcode audit: host→box MGMT `0x01`-`0x05` taken, box→host `0x81`/`0x82` taken; `0x06` is free. CT_*
is taken through `0x18` (`CT_PHONE_IDENT`); this proposal adds no CT_*.

ACK: the existing `[MGMT_ACK][0x06][status]`, `0` = stored, `1` = malformed. Correlation is the
echoed verb byte only — there is no sequence token.

### 4.2 `ocbmd`

- `PREFERRED_PHONE_FLAG: &str = "/tmp/preferred_phone"`, beside `RADIO_OFF_FLAG`, same ownership
  model: "an app-commanded surface, not an on-box lever".
- Validation, in order: payload empty (clear) or exactly 17 bytes → else `status 1`; six 2-hex
  octets colon-separated → else `status 1`; normalize to uppercase to match `bonded_macs()`'s
  `{:02X}`. Membership is **not** required (D8).
- Write with the existing atomic `.tmp` + rename so the wireless side can never read a torn MAC.
- **Clear it** in: `go_idle()` (next to `remove_file(RADIO_OFF_FLAG)`), at ocbmd startup, and in
  **both forget arms** — `MGMT_FORGET_DEVICE` when the MAC matches, `MGMT_FORGET_ALL` always. That
  last one is the stock firmware's recorded bug (§2.6); do not repeat it.
- **Do not clear on `CT_SUBSCRIBE`.** That would open a window where a reconnect round runs between
  SUBSCRIBE and the app's re-push with the preference lost. Clearing only in `go_idle` also inherits
  `STOP_GRACE` semantics for free.
- `box_info_json` gains `"preferred": "<MAC>"` (or `""`), sourced with `read_trim` — the pattern the
  snapshot already uses for `/tmp/carplay_transport` and `/tmp/phone_present`. The value is validated
  hex+colons so it needs no JSON escaping.

### 4.3 `carplay-wireless`

Two **pure, named** functions (agent 12 needs them as test seams):

```rust
// ⚠️ CORRECTED 2026-08-16: these two seams ALREADY SHIPPED in c4d3a07, under different names,
// in crates/vendor/wireless/src/control.rs — `parse_addr` and `order_bonds` (+ Control::ordered_bonds).
// They are pure free functions and already carry this plan's own golden tests: T13 is
// `connect_with_an_address_parses_into_mgmt_order`, T12 is
// `policy_order_is_honoured_but_never_hides_a_new_bond`. Do NOT rebuild them.
fn parse_preferred(s: &str) -> Option<[u8; 6]>          // display MAC -> little-endian bdaddr
fn order_for_reconnect(bonds: Vec<[u8;6]>, preferred: Option<[u8;6]>) -> Vec<[u8;6]>
```

Called at `reconnect.rs:127`, beside the existing per-round `bonded_addrs()` re-read:

```rust
let mut bonds = ssp_agent::bonded_addrs();
if let Some(pref) = preferred_peer() {            // per-round read of /tmp/preferred_phone
    if let Some(pos) = bonds.iter().position(|b| *b == pref) {
        let p = bonds.remove(pos);
        bonds.insert(0, p);                       // stable: others keep file order
    }
}
```

**Byte-order contract:** the file carries display form `AA:BB:CC:DD:EE:FF`; `bonded_addrs()` returns
little-endian bdaddrs (`ssp_agent.rs`, `pub fn bonded_addrs` — contract documented at `:144-147`,
body `:148-157`: "Returns each record's 6-byte bdaddr in the stored mgmt little-endian order"), so
`parse_preferred` reverses the octets. This is the single most likely bug in the feature — see the
test discipline in §7.

Everything else stays untouched: `CONNECT_TIMEOUT_SECS=8`, `BACKOFF_START_SECS=10`,
`BACKOFF_MAX_SECS=60`, `INITIAL_SETTLE_SECS=5`, the `session_active` `compare_exchange` claim, and
critically the audit-B5 floor that **always** sleeps ≥ `BACKOFF_START_SECS` between rounds. Apply the
preference as a reorder only — never as an inner "retry preferred N times" loop that would bypass it.

**Demotion:** a counter of consecutive preferred-peer attempts that reached no iAP2 milestone; at
N=3, skip the reorder until a successful session or a preference change.

**Timing** (agent 4): preferred absent, another bonded phone present → boot costs 5 s settle + ≤8 s
timeout + the other phone's connect ≈ **13-16 s** (vs ~6 s unprefaced). Mid-backoff arrival: ≤68 s
worst case, of which the preference contributes the fixed 8 s. Both phones present: preference costs
nothing and wins.

### 4.4 Android persistence

**Mechanism: SharedPreferences, one versioned JSON blob, `commit()` on a store-owned executor.**

- Not DataStore: it was deliberately removed from this project with a written rationale
  (`build.gradle.kts:154-157`, "write-only dead I/O"), and Preferences DataStore has no list type so
  it would store the same JSON string anyway.
- Not Room: schema/DAO/ksp machinery for ≤10 records with no queries.
- `commit()` not `apply()`: a head-unit power cut is the *normal* shutdown, and `apply()`'s async
  flush can be lost. SharedPreferences writes via temp-file + rename with a `.bak`, so a mid-write
  kill yields the previous good file.
- `allowBackup="false"` (`AndroidManifest.xml:46`) makes "cleared by clear-app-storage" hold with no
  cloud restore resurrecting the list.

```json
{"v":1,
 "preferredMac":"aa:bb:cc:dd:ee:ff",
 "devices":[{"mac":"…","name":"<owner> iPhone","model":"iPhone18,4","osName":"iPhone OS",
             "osVersion":"27.0","firstSeenMs":…,"lastConnectedMs":…,"bonded":true}]}
```

`preferredMac` is a **single top-level field**, not a per-device flag — that structurally guarantees
"at most one preferred" with no invariant to police.

**Threading.** One `@Volatile` immutable snapshot; all reads (including PhonesTab's 1 Hz poll) are a
volatile read, never disk. Load once at `initialize()`'s one-time block on IO. Writes are
write-through on the **store's own** single-thread executor — *not* the manager's `scope`, because
`release()` cancels `scope` before teardown and would silently kill an in-flight final write (the
same swallowed-post hazard the wake-lock comment documents). Storage I/O never blocks `startImpl()`,
which touches only the in-memory `@Volatile preferredMac`.

### 4.5 `CarlinkManager` integration

Current flow, for reference: exactly two refresh triggers — `PhonesTab` opening
(`PhonesTab.kt:98`) and the post-forget re-poll (`CarlinkManager.kt:1060-1064`). No periodic poll, no
on-connect poll. `pollBoxInfo` builds `DeviceInfo` from bare MACs and commits **wholesale** via
`mutateDeviceList`.

New state, all under the existing `deviceListLock`: `knownDevices: Map<String, KnownDevice>`,
`@Volatile preferredMac: String?`, and **`@Volatile bondedMacs: Set<String>`**.

New public API (additive only — the public surface is deliberately stable). **Both LANDED in Phase 1,
2026-08-16, with ZERO callers** (`CarlinkManager.kt:343` and `:1748`; nothing in `app/src` reads or
calls either). Phase 2 must WIRE these to the card tap — do not re-add them:

```kotlin
val preferredBtMac: String?
fun setPreferredDevice(btMac: String?)
```

**Merge rule** (replaces only the body passed to `mutateDeviceList`; the wrapper and its suppression
filter stay untouched): display list = (bonded MACs, `bonded=true`) ∪ (persisted devices not in the
snapshot, `bonded=false`); names resolved `phoneNames[mac] ?: knownDevices[mac]?.name ?: mac`.

**Five regressions to avoid, in priority order:** *(**STATUS 2026-08-16 — all five are CLOSED.** #1 was already marked FIXED; #2-#5 all landed in Phase 1 `08fec2f`: forget-deletes-record, every merge routed through `mutateDeviceList`, MGMT_INFO-authoritative `bondedMacs`, and both notification channels. Likewise `DeviceInfo.bonded`/`lastConnected`, described later as future work, are set by `mergeDeviceList` and rendered in `PhonesTab.kt` — §8 of this file already says so.)*

1. ~~**`_connectedBtMac` breaks for single-phone users.**~~ **FIXED in Phase 1, 2026-08-16**, exactly
   as prescribed: the heuristic now runs over `bondedMacs` and `connectedPhoneMac` is preferred over
   it (`CarlinkManager.kt:1008-1015`), so `connectedPhoneMac` is no longer write-only (set at
   `:1660,1671`, read at `:1008`). Kept here as the record of why the merge is shaped this way.
2. **Forget must delete the persisted record too.** `recentlyForgotten` is in-memory and
   `elapsedRealtime`-based; it cannot cover storage, so the device would return after
   `FORGET_SUPPRESS_MS` (25 s) or on next app launch.
3. **All merges must go through `mutateDeviceList`**, or the box's stale snapshot during its ~4-5 s
   wireless restart resurrects a just-forgotten device **into permanent storage**.
4. **MGMT_INFO stays authoritative for bonded membership.** Never re-mark a MAC bonded from
   persistence alone, or a device forgotten out-of-band renders as connectable forever.
5. **Both notification channels** must fire from new mutation sites — `callback?.onDeviceListChanged`
   is public API even though only `DeviceListener` has consumers today.

**`DeviceInfo`: keep, extend additively.** Add `bonded: Boolean = true` (defaulted, so every existing
construction site compiles) and populate `lastConnected` — which lights up `PhonesTab.kt:410`'s
existing "Last seen" rendering with **zero UI changes**, dead code today only because the field is
always null. Do **not** add a `preferred` flag: it would make the `refreshDeviceNames` equality
short-circuit sensitive to preference churn. Expose `preferredBtMac` as a property instead, exactly
how `activeBtMac` already works.

### 4.6 UI

**Card tap becomes SELECT, not connect/disconnect.** Today tapping the connected card calls
`disconnectPhone()` with **no confirmation** (`PhonesTab.kt:179-187`) — one accidental tap kills
CarPlay. Disconnect moves to an explicit button on the connected card, with a confirm.

- No live session → tap sets preferred and attempts a connect. No dialog; nothing is disrupted.
- Live session on another phone → confirm: "Switch to X? The current CarPlay session with Y will
  end." Cancel changes nothing (no half-applied preference).

**Three orthogonal visual channels**, so states compose:

| State | Channel |
|---|---|
| Connected now | background tint (existing green glass) |
| Preferred | star chip "Auto-connect" + 2dp ring + always first |
| Merely known | neutral glass, status line "Last connected: …" |
| In history, not bonded | "Not paired with adapter", tap disabled |
| Bonded, no name yet | title "iPhone", subtitle the MAC; self-heals on first `CT_PHONE_IDENT` |

Preferred never uses tint — tint stays reserved for "live now", so preferred-and-absent reads
correctly.

**Ordering rule:** explicit selection → most-recently-connected → first-seen. This collapses all
three of the requirement's phrasings: with one phone it is "the first device"; if the user picked
one it is "the device selected"; otherwise "last-connected". Made visible by always putting the
starred card leftmost, so "which phone will it pick?" is answered by position, not by inferring a
rule. Re-sort only on load and on explicit selection — never spontaneously under a moving finger.

**Remove:** one button per card doing both stores (users have one mental model), with the disclosure
in the confirm dialog. The aggregate "Clear device history" beneath the row is where the two stores
are separable.

**Driving safety:** park-gate Remove and Clear-history (destructive and never urgent); keep
switch-with-confirm available while driving for the passenger case. `driveMonitor` is private
(`CarlinkManager.kt:349`) and needs a read accessor. Cap rendered history (~6) so the row can never
become the kind of scrollable list UXR restricts. `driving` defaults false until the car service
answers — fail-open, acceptable for an advisory gate.

---

## 5. Failure modes

| # | Risk | Trigger | Guard |
|---|---|---|---|
| FM1 | Radio-stack bounce kills the projecting phone | Preference verb reuses `request_wireless_restart()` | Data-only write; per-round read (D4) |
| FM2 | Flap detector → L3 reboot ×3 | Preference applied via `/tmp/host_present` dip; 5 toggles in 20 s | Nothing in the preference path writes that flag (D4) |
| FM3 | Preferred phone stalls, starving others for 120 s | Preferred accepts DLC, never completes iAP2 | Ordering not filtering + demotion at N=3 (D3) |
| FM4 | Dangling preference after forget | Forget does not clear it (the stock bug, §2.6) | Clear in both forget arms **and** validate per round — two layers, self-healing |
| FM5 | Truncated write on a full rootfs | jffs2 near full | Moot under D2 (tmpfs). jffs2 *wear* is **not** a real risk for a <100 B human-toggled file |
| FM6 | Paging degrades inbound page-scan | Repeated attempts against an absent phone | **UNCERTAIN** — not answerable from this repo. Keep the existing backoff floor and cap unchanged for the preferred peer |

**Regression paths that could block a previously-working phone** (all closed by D3+D4+FM4):
filter-style targeting removing other bonds' reconnect; the 120 s slot capture dropping their inbound
connects; a dangling preference idling reconnect under a filter design; a restart-based apply killing
the currently projecting phone.

**Safe default with no preference:** exactly today's behaviour — try every bond in stored order.
Absence of the file must parse as "no preference" (fail-open), never as an error or an empty
allow-list.

**Logging for "my phone won't connect any more":** on the existing `[reconnect]` channel — the
preference value and its source on change or dangle; per attempt, whether the target was preferred or
fallback and the failure stage (SDP / RFCOMM / iAP2); demotion events with the counter. Surface
`preferred` alongside `devices` in `MGMT_INFO` so the CCPA tab shows both.

---

## 6. Compatibility and deployment

**Unknown-message behaviour, quoted:** ocbmd unknown channel `_ => {}` (`main.rs:2358`); unknown MGMT
verb `_ => {}` (`main.rs:2060`, **no ACK**); unknown CT_* falls off the if/else chain. macOS
`default: logUnhandled` (`OCBMClient.swift:651-655, 689-692`). Android `else -> log.i(...)`
(`OcbmClient.kt:431`). **Nothing treats an unknown message as fatal anywhere.**

| Combination | Result |
|---|---|
| New app + old box | Verb dropped, no ACK, app times out (5 s). Eliminated entirely by the `CAP_KNOWN_DEV` gate |
| Old app + new box | No preference sent → box behaves exactly as today. MGMT_INFO safe **iff additive** |
| New app + new box | Feature active |

Precedent that new box→host traffic is benign: the mac app has **no cases at all** for
`ctPhoneIdent` (0x18) or `ctBtPhase` (0x17) — its CT list stops at `ctRadio` 0x16 — so the box
already sends CT ops the mac app merely throttle-logs.

**Deployment order: box first, apps second.** "Old app + new box" is the benign row. Push ocbmd +
carplay-wireless together in one reboot to avoid a mixed window. The macOS app needs no change at all
provided MGMT_INFO stays additive.

**Rollback: app-only, and D2 is what makes it work** — there is no persisted box state to outlive it.
Ship the explicit *clear* form (empty payload) so the escape hatch exists from any app version, and
fold "clear preference" into `MGMT_FORGET_ALL`.

---

## 7. Test plan

~20 JVM tests, **zero emulator tests**, 5 hardware checks. Every JVM test has a named one-line
mutation that must kill it. (The ids below run T1-T5, T7-T13, T15-T20 — **18 in all; there is no T6
and no T14 anywhere in this document**, so the gaps are numbering, not missing text.)

Emulator tier is empty on purpose: the androidTest source set does not exist, the emulator has no
Bluetooth and no USB host, and Robolectric already exercises real SharedPreferences file semantics —
an instrumented store test would test AOSP, not us.

**Design prerequisite:** the pure functions in §4.3 must exist as named seams. **The merge half
LANDED 2026-08-16 (Phase 1):** `com.carlink.device.mergeDeviceList(known, bondedMacs, suppressed,
preferredMac, formatLastSeen)` (`KnownDevices.kt:133`) is extracted, pure, and covered by
`KnownDevicesTest` (12 tests). Note the time seam shipped as `formatLastSeen: (Long) -> String`
rather than an injected `now`. Still to build: the Rust seams `parse_preferred` /
`order_for_reconnect` (§4.3).

### JVM — wire (Kotlin, over `FakeTransport`)

- **T1** Golden request bytes, verb hand-written as literal `0x06`, plus one assert pinning
  `Ocbm.MGMT_SET_PREFERRED == 0x06`. *Mutation:* change the const; lowercase the MAC.
- **T2** Full-envelope golden (16-byte header, `channel = 40 00` LE, hand-computed hcheck).
- **T3** ACK round-trip, status 0 and 1. *Mutation:* swap `r[2]` for `r[1]`.
- **T4** A stale ACK for a *different* verb must not satisfy the call. *Mutation:* change
  `r[1] == verb` to `true` — **only this test fails**.
- **T5** Malformed inbound MGMT frames never throw (dispatched on the read thread; an exception kills
  the loop). Truncated, empty, 64 random bytes; assert the client still answers a later good ACK.
- **T7** Preference is **re-pushed on session start** with no UI action. *Mutation:* delete the push
  from the connect path. This is the test most likely to catch a real field failure.

### JVM — wire (Rust, ocbmd `mod tests`)

- **T8** `assert_eq!(p::MGMT_SET_PREFERRED, 0x06)` — with T1, makes cross-language drift impossible.
- **T9** `set_preferred(mac, path)` path-parameterized (as `forget_one_bond`'s hardcoded const is
  untestable today): normalized uppercase, atomic rename. *Mutation:* drop the rename.
- **T10** Malformed args → `status 1` **and no file written** (pre-create, assert unchanged).
  Unbonded-but-well-formed → `status 0`, stored (D8).
- **T11** Both forget arms clear the preference. *Mutation:* delete the one clear line.

### JVM — ordering (Rust, wireless crate)

- **T12** `order_for_reconnect`: preferred second-of-three → index 0, others keep relative order,
  length unchanged (kills both "appended" and "duplicated"); not-in-bonds → identity; `None` →
  identity; empty → empty.
- **T13** `parse_preferred` golden byte order: hand-typed `"AA:BB:CC:DD:EE:FF"` →
  `[0xFF,0xEE,0xDD,0xCC,0xBB,0xAA]`. **Do not** build the expected value with the production
  display-formatter.

### JVM — persistence and merge (Kotlin)

- **T15** Serialized-form golden (hand-typed literal) **plus** a reader-only fixture the writer never
  produced. Include a name with `"`, `\`, emoji and a newline — names are user-typed on the phone.
- **T16** Survives a process restart: write via store A, construct a fresh store B over the same
  `Context`, contents identical. *Mutation:* cache in a `companion object` static — still passes T16,
  fails T17, which is why both exist.
- **T17** All state in exactly one prefs file, no statics.
- **T18** Merge semantics: ∩ → named; box-only → learned; history-only → present but **not bonded**
  (history must never render as a live bond); suppressed → excluded.
- **T19** The just-forgot race, **both edges**: suppressed inside the window; reappears after it
  (box truth wins — a resurrection there is *correct* and means the box failed the forget); and a
  `CT_PHONE_IDENT` for a suppressed MAC lifts suppression immediately, so forget-then-re-pair inside
  25 s does not stare at an empty list.
- **T20** Forgetting the preferred device clears the local preference.

### Hardware checklist

- **H1** Preference does **not** survive a box reboot (asserting D2's intent), and the app re-asserts
  it on the next session.
- **H2** ocbmd → carplay-wireless handoff: SET_PREFERRED then observe `[reconnect]` log order.
- **H3** **Acceptance test.** Two bonded iPhones: preferred one paged first, **and the non-preferred
  one still connects when the preferred is absent**.
- **H4** Forget-preferred on hardware: loop does not page it; ACK sequencing survives the wireless
  restart.
- **H5** App-kill / relaunch: the known list renders from the store before the box answers
  `MGMT_GET_INFO`.

### Tautology discipline

The house rule is stated three times already (`OcbmFramingTest.kt:12-16`, `BinaryPlistTest.kt:9-14`,
`hid.rs:110`). Three places it will try to sneak in here:

1. **Verb bytes** — hand-write `0x06`; asserting against the constant tests nothing about the number.
2. **MAC byte order** — hand-type both forms. A shared reversal error is the single most likely bug.
3. **Store round-trip** — `read(write(x)) == x` passes for any self-consistent format, including one
   a future version cannot read.

**Not worth testing:** `FakeTransport` itself, Robolectric's prefs implementation, that OS clear-data
deletes files, reconnect backoff timing (untouched, real-time sleeps, guaranteed flake), Compose
rendering (no infra in `test/`; T18 pins the merge), and existing FORGET/GET_INFO behaviour beyond
the new interplay.

---

## 8. Work plan

**Phase 1 — LANDED 2026-08-16, hardware-verified.** `KnownDeviceStore`; the merge;
`bondedMacs`; forget deletes the persisted record; wire `connectedPhoneMac` into `_connectedBtMac`;
`DeviceInfo.bonded` + populated `lastConnected`. Value on its own: the list persists, populates before
any connection, and shows real names instead of hex after every restart. Fixes two live bugs.

**Phase 2 — UI.** Tap-to-select; the three visual channels; explicit Disconnect with confirm;
park-gated Remove/Clear-history. **The ordering COMPARATOR landed in Phase 1** — `mergeDeviceList`
sorts preferred → most-recently-connected → first-seen → MAC (`KnownDevices.kt:155-161`), the exact
§4.6 precedence — but nothing can set a preference yet (`setPreferredDevice` has no callers), so the
preferred-first arm is unreachable. Phase 2 owes the card tap that sets it and the star chip that
makes the resulting order legible.

**Phase 3 — box.** `MGMT_SET_PREFERRED` + `CAP_KNOWN_DEV`; ocbmd verb arm, flag, lifecycle, forget
coherence, `"preferred"` in MGMT_INFO; `parse_preferred`/`order_for_reconnect` + demotion in
`reconnect.rs`; fix the host-side `mgmtLock` gap (`OCBMANDROID.md`, "The host-side `mgmtLock` gap"
under "Not done yet"; `OcbmClient.kt:925,939`) — two concurrent MGMT
actions can steal each other's ACK, and this feature puts a MGMT verb behind a UI tap.

---

## 9. Known unknowns

Carried deliberately; none blocks Phase 1 or 2.

- Whether an accessory-initiated connect to a phone that is bonded but **never CarPlay-consented**
  re-raises the consent card, silently degrades to classic BT, or is refused.
- What iOS does when a second known car connects mid-session with another.
- The exact predicate inside `isWirelessCarPlayAllowedForCertSerial:` (only the XPC method name is
  evidence).
- Whether paging an absent phone measurably degrades inbound page-scan on these chipsets.
- Whether a kernel/controller cap on loaded link keys exists — nothing in this tree caps bonds and
  there is no eviction; a cap would more likely surface as a rejected `LOAD_LINK_KEYS` (which is
  logged) than as silent eviction.
- R14G17 is silent on the entire BT transport layer (2017, Bonjour-only), and CarPlaySDK.framework is
  wired-only — the BT-layer knowledge here rests on this project's device captures plus iOS binary
  strings, not on licensed source text.

## 10. What "Remove" means — RESOLVED 2026-08-16, landed

Owner decision: **Remove forgets the phone from both the app and the box, forcing the next connection
from that phone to be a fresh pairing.**

That needed a box change, because clearing the BR/EDR bond alone is not a fresh pairing: the phone
redoes Bluetooth SSP, but its AirPlay long-term key survives in `/etc/carplay_peers.bin` and the next
session takes the fast pair-verify path. The box would keep a 32-byte key for a device the user asked
it to forget.

**Both forget verbs now clear the whole AirPlay peer store.** Per-device removal is not possible: the
store is keyed by the controller's AirPlay pairing identity (the `IDENTIFIER` TLV from pair-setup M5,
`pairing/src/setup.rs:199`), not by the BR/EDR MAC, and that id never leaves the pairing crate — it is
local to `verify.rs:162`, surfaced to neither airplayd's connection state nor the receiver's SETUP
handler. Answering "which LTPK belongs to this MAC" would need a mapping recorded at SETUP.

Chosen trade: clear all. Other bonded phones redo pair-setup on their next connect — the slow path,
**not a prompt**, since pair-setup is MFi-authenticated and automatic. One slightly longer reconnect,
nothing the user sees. If a multi-phone household ever notices, the refinement is to plumb `peer_id`
out of the pairing crate and record `deviceID -> peer_id` alongside the identity already published at
SETUP.

Deleting the file is sufficient even though a running airplayd holds the pairings in memory and
`save_peer` persists the WHOLE map: both callers request a wireless restart, and `wireless_down` reaps
airplayd (`pkill -f "[a]irplayd"`) whenever the wireless session owns it, so it reloads from the absent
file — which is also its normal cold-start case.

Verified on hardware, reversibly: both stores backed up, Remove issued from the UI, `bt_link_keys`
observed at 0 bytes and `carplay_peers.bin` gone with `[ocbmd] mgmt: cleared AirPlay pairings`, then
both restored and a full session re-established.

**Phase 2 follow-up:** the confirm dialog still reads "The adapter will no longer auto-connect to this
device." That undersells it now — it should say the phone will need to be paired again.

## 11. What NOT to carry forward from the stock implementation

- **Adapter-owned list with a detached preference pointer** — this is what produced the
  `LastConnectedDevice` bug (§2.6).
- **Implicit last-connected-wins as the only policy** — stock had no user-designatable preferred
  device at all.
- **Connect-on-boot with no app in the loop** — explicitly superseded by docs/carplay/04_CAPABILITIES_AND_CONFIG.md; radios are
  app-gated.
- **The 0x12 paired-list wire format** (unseparated MAC+name needing regex recovery) and the
  **dual-purpose 0x11 type id**.
- **Slow, unacknowledged forget** (10-20 s, no completion signal). Ours ACKs.
