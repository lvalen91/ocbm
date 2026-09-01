# Testing — live session plans and gates

> **STATUS:** CURRENT · single owner for this topic. Consolidated 2026-08-31 from pre-consolidation docs 46; the originals are in git history and in the 2026-08-31 backup. Correct this file in place — do not add a sibling.

What must be exercised on hardware, and the order that makes a failure legible.

## Live session test plan

<!-- absorbed: ../ops/02_TESTING.md -->

Status: executed 2026-07-25. The `'cmnd'` fix held — `RX 0xAA00 -> CertSent` present, SYN-ACK count 2,
full identification and metadata. Retained as the regression checklist for any change to the wireless
transport. Sections describing hypotheses that the run resolved are marked below.

Companion to `docs/carplay/05_METADATA_AND_CONTROLS.md`.

### The discriminator — one grep, box-side, no phone trace needed

```sh
./host/uart_cmd.sh 'grep -c "RX 0xAA00 -> CertSent" /tmp/airplayd_wl.log' 6
```

`0xAA00` is `RequestAuthenticationCertificate` — the **device's** first move after it accepts the link.
The phone cannot emit it while still retransmitting SYN-ACK. `1` ⇒ our ACK now reaches the phone. `0` ⇒
it still does not.

Faster equivalent, fires ~10 ms earlier: any inbound frame with `ctrl=0x40` (i.e. anything that is not
another SYN-ACK) proves the phone's link layer advanced.

```sh
./host/uart_cmd.sh 'grep "datastream. RX" /tmp/airplayd_wl.log | grep -c "ctrl=0x40"' 6
./host/uart_cmd.sh 'grep -c "ctrl=0xc0" /tmp/airplayd_wl.log' 6   # 1 = accepted; 31 = old behaviour
```

> **⚠️ THE THREE `ctrl=` GREPS ABOVE ARE DEAD — they read 0 on a HEALTHY box**, and a 0 on the
> `ctrl=0xc0` line reads as *better* than "accepted". Both log lines are gated behind
> `CARPLAY_EVENTS_LOG`, which no spawn site sets. Use instead
> `grep -c 'outbound sink registered' $L` ≥ 1 (sink installed) and
> `grep -c 'SETUP phase2 DataStream(130)' $L` (phone asked). [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-46-1`.

Deliberately NOT the discriminator: `[datastream] TX(RCS,'cmnd')`. That only proves we wrote the right
bytes, which was never in doubt.

### Second hypothesis — the write key has never been verified (resolved: the write key is correct)

`'cmnd'` is not the only cause that produces total silence. `probe_datastream_keys` authenticates the
**read** direction only; the write key is *inferred* as the other half of the HKDF pair. Evidence
previously cited as confirming the write path (`iAP2PacketParseBuffer, dataLength: 6/26/9`) is now known
to be Bluetooth and `POST /command` traffic — **nothing we have ever written to the DataStream has been
observed to arrive.** A wrong write key and a wrong message type are indistinguishable from our side.

**Separating experiment (free, zero risk, one extra frame):** send one frame with a deliberately invalid
transport kind at `0x04` (e.g. `'junk'`). Apple's `controlServer_receiveData` rejects an unknown kind
with **`-6717 kFormatErr`** — but it can only do so *after decrypting*. So:

- phone logs `-6717` (or any reaction at all) ⇒ **the write path and crypto are correct**; if the link
  still stalls, `'cmnd'` or something above it is the remaining fault.
- total silence ⇒ **the frame never decrypted**; the write key/direction is the fault, not the type.

Run this only if the primary discriminator says the ACK still is not landing. It costs one frame and
converts an ambiguous negative into a decisive one.

### Next hypothesis if the primary fix is insufficient — `'sync'` at offset 0x04 (not needed; `'asyn'` is accepted)

If the ACK still does not land after the `'cmnd'` fix, **try `'sync'` before anything else.** It is a
two-line change and it is what Apple's own accessory reference does.

`CarPlaySDK::_AirPlayReceiverSessionSendiAPMessage` @ `0x272a9c` checks
`AirPlayAccessoryEnabledFeatureIAP`, and when enabled routes to the **DataStream** (independently
confirming docs/carplay/05_METADATA_AND_CONTROLS.md's whole thesis) with `w1 = 1` — "synchronous". That threads through
`_AirPlayReceiverSessionDataStreamSend` → `SendInternal` → `APMediaDataControlServerSendRequestSync` →
`'sync'` + an 8-byte random messageID + a **10 s** wait (`0x2540be400` ns). For contrast,
`VehicleDataProtocol1Send` passes `w2 = 0` → `'asyn'`. Both kinds are first-class; **Apple picked
`'sync'` for iAP specifically.**

`'asyn'` is judged legal and is the right choice for the first run: `startMessageHandling` never
inspects the kind at all (its only identity branch is the `'died'`/`'cmnd'` filter), it explicitly
supports the reply-less case (`cbz x21` guards the out-reply store), and the phone sends *us* `'asyn'`
on this very channel. Shipping it also avoids a 10 s blocking round-trip per frame.

**Honest gap:** the phone-side code that parses our `0x04` lives in AirPlaySupport, which extracted
without a usable symbol table, so the "`'asyn'` is accepted" conclusion is inference from the layers
above and below — strong, but not the same evidence class as the `'cmnd'` filter itself.

**Note for branch B:** switching to `'sync'` would NOT have exposed the `'comm'` bug. Both the accept and
reject paths leave the out-reply null with OSStatus 0, so a `'rply'` would have come back empty either
way. There was no way to see this from the wire — only from disassembly.

### Carrier discriminator (which carrier a frame actually took)

`airplayd`'s `Event message received from 192.168.43.1:PORT, … Body N bytes, ID 0xNNNN` is the phone
receiving an accessory `POST /command`. The plist overhead is constant, so the body size names the frame:

| iAP2 frame | plist body |
|---|---|
| 6-byte DETECT | **94 B** |
| 26-byte SYN | **116 B** |
| 9-byte ACK | **97 B** |

Use this to prove which carrier carried what. In sess3 the four bodies were `94, 116, 94, 116` and **no
97-byte body existed** — both DETECT+SYN pairs went over `POST /command`, and only the ACK took the
DataStream.

**Corollary, contra docs/carplay/05_METADATA_AND_CONTROLS.md's framing:** `POST /command` is a *working outbound carrier on iOS 27*, not a
dead 2017 mechanism — it delivered every DETECT and SYN in both archived sessions. Only the **inbound**
direction requires the RCS channel.

### Pre-flight gates (do these before starting a session)

1. **Binary identity.** `md5sum /usr/sbin/airplayd` must equal the packed artefact you built. A run
   against a stale binary is worse than no run.
2. **Runtime build identity.** ~~`grep -c "TX(RCS," $L` ≥ 3~~ — **DEAD, corrected 2026-08-16**: that
   line is gated behind `events_log()` and `CARPLAY_EVENTS_LOG` is never set, so it fails on a CORRECT
   binary. Use `grep -c "outbound sink registered" $L` ≥ 1 (ungated, `datastream.rs`) instead;
   the old one logged `TX iAP2`. Non-zero `grep -c "datastream. TX iAP2 "` ⇒ wrong build.
3. **`is_iap` must be true.** If `clientTypeUUID` is absent from the SETUP request the sink is never
   registered, every send falls back to `POST /command`, and the fix is inert while looking like a
   failure. Confirm the `DataStream(130)` line ends `[iAP channel]`.
4. **Phone trace liveness — without spending a session.** Plug any MFi wired accessory (EarPods,
   USB-C→3.5 mm adapter, car cable) in for ~10 s while running
   `idevicesyslog -u <UDID> -p accessoryd --no-colors -o pre.txt`, then `grep -c "LOG;" pre.txt`.
   `> 0` ⇒ `PrintIapPackets` is live, proceed. `0` with accessoryd lines present ⇒ the pref is off;
   reinstall the CarPlay/iapd profile, **reboot**, retest.
   **Do not trust `capture_iphone_carplay.v2.sh`'s PRECHECK** — accessoryd is silent when idle, so it
   warns unconditionally. Measured: 55,001 system-wide `<Debug>` lines with zero from accessoryd.
5. **Arm the log-truncation guard.** `session_supervisor.sh::bound_logs` tail-truncates
   `/tmp/airplayd_wl.log` to 64 KB once it passes 256 KB — destroying exactly the handshake window.
   Snapshot at ~T+30 s: `cp /tmp/airplayd_wl.log /tmp/hs30.log`.
   **`/tmp` is tmpfs — pull it off the box before anything reboots.**

### The trace persists — pull it retroactively

The `LOG;` trace lives in the on-device log store; 226 lines were recovered from a two-hour-old window.
This removes the start-the-capture-first fragility entirely.

```sh
idevicesyslog -u "$UDID" archive post.tar --age-limit 1800 && tar -xf post.tar -C post.logarchive
/usr/bin/log show --archive post.logarchive --style compact --info --debug \
  --start '<T0 minus 30s>' --predicate 'process == "accessoryd"' > accd.txt
tools/extract_iap2_trace.sh accd.txt
rm -rf post.tar   # ~290 MB
```

The row to look for: `AirPlay  Acc  0x40  ACK` — the phone confirming, in its own words, that our ACK
arrived. Cross-check the summary: `AirPlay iPod SYN-ACK` must be **1**, not 31.

Note `/usr/bin/log` explicitly — a shell function shadows `log` in this environment and swallows args.

### Negative checks — all must be zero

`datastream. TX iAP2 ` (both = old binary) · `key probe EXHAUSTED` ·
`RX 0x1D03` · `[datastream] decrypt FAILED` · `envelope did NOT parse` (a reassembly failure; `drain_rcs` should make this
impossible. Inbound framing is **16384**, not 1024 — `MAX_WRITE = 1024` is our outbound chunking, while
the phone's DataStream path uses `NetSocketChaCha20Poly1305Configure` at 16 KB and `MAX_READ` matches
it) · `[art] INCOMPLETE` · `TX failed after retries` ·
`refusing to install gen` · `RX 0x1D03` · `tunnel session aborted`

New strings worth reading every session:
`[features] metadata policy (resolved once): param6=… param7=… subscribe=… skip=…` — the ONLY way to
confirm which tier a running process actually pinned; it is resolved once per process, so editing
`/tmp/carplay_metadata` mid-run does nothing · `[features] dropping X — it rides Y, which is not
active` (a rider was orphaned by a skip and correctly dropped) · `[art] OVERLONG id=… > declared …`
(duplicated artwork fragment; see docs/ops/05_AUDITS.md §4) · `[meta] seam write failed — reconnecting on next
message` (was the sink-eviction bug, fixed 2026-07-29 — should no longer appear).

> Note (2026-08-10): the `/tmp/carplay_metadata` / `CARPLAY_METADATA` levers this plan drives are
> interim, box-side controls — per docs/carplay/04_CAPABILITIES_AND_CONFIG.md tier ownership is app-side and the levers migrate to
> app-pushed config. Every operational instruction in this plan (the tier-confirmation log line, the
> `echo proven` recovery, the post-reboot re-arm) remains exactly how the CURRENT build is driven;
> follow them unchanged until the migration lands.

Informational, record but not failures: `SETUP phase2 stream type N NOT IMPLEMENTED` (write the list
down — each is a candidate for the next stream-130-class discovery), `stale connection (gen N) closed`,
`resent SYN only`.

### Decision tree

- **`RX 0xAA00 -> CertSent` present** → `'cmnd'` was the blocker; docs/carplay/05_METADATA_AND_CONTROLS.md §6.1 closed. Follow the log
  forward; whatever stops first is the new frontier.
- **Still many `ctrl=0xc0`, no `0x40` inbound** → *(both counts are now permanently 0 regardless of
  health — see the 2026-08-16 correction above; judge by `outbound sink registered` instead.)* refuted.
  First check `grep -c "outbound sink registered"` ≥ 1: if the
  ACK was never written, the bug is upstream (no sink), not on the wire. Surviving hypotheses: the
  `0x08`/`0x1c` fields are not reserved; `'asyn'` is wrong for accessory→phone (try `'sync'` + `'rply'`);
  our seq/ack values. **This branch requires the phone trace** — box logs cannot separate them.
- **`0xAA00` then `0x1D03`** → transport, link and MFi auth all correct; only the Identify payload is
  wrong. A good outcome. Decode the reject body; do **not** reflexively re-add 0x4157/0x4170.
- **Identified but no metadata** → check whether every subscribe logged `→ sent` (8 under `extended`, 3 under `proven`). Have Apple Music
  **playing before** the session so an idle phone cannot cause this.
- **`RX 0x1D03`** → do NOT bisect. Capture the phone's reason:
  `idevicesyslog -u <udid> -p accessoryd -o /tmp/rej.txt`, then
  `grep -E "iapreject|Identification info rejected" /tmp/rej.txt`. It names the param, the message id
  and a reason from Apple's enum (docs/carplay/05_METADATA_AND_CONTROLS.md §6.5). Recovery is `echo proven > /tmp/carplay_metadata`.
- **Identified but only NowPlaying/RouteGuidance** → check `/tmp/carplay_metadata` says `extended`;
  `/tmp` is tmpfs and a reboot reverts to the baseline. Confirm the subscribe count line
  `[events] iAP2-tunnel metadata: N subscribes`.
- **Metadata but no artwork** → confirm session 2 is routed (`RX session-2 file-transfer fragment`) and
  that each transfer reaches `[art] complete id=0x… N B (expected N)`. `[art] INCOMPLETE` means
  fragments were lost; `envelope did NOT parse` means reassembly failed.
- **No stream-130 SETUP at all** → the phone never opened an iAP channel; look at `/info` and the
  `enabledFeatures` echo, not at anything this round changed.
  **⚠️ Before concluding "at all", check whether the SETUP is ARRIVING and being REFUSED** — this row
  sent the 2026-08-10 diagnosis in the wrong direction. The phone was asking 33 times a session and the
  box was rejecting it, which is a different failure from the phone never asking.

  > **⚠️ THE GREP THIS ROW USED TO PRESCRIBE, `grep -c "SETUP stream type=130"`, CAN NEVER MATCH** —
  > 130 is exempt from the guard that emitted it, so the count is permanently 0, which this row then
  > reads as "the phone never asked". Use the two greps below instead. Full reasoning:
  > [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-46-2`.

      grep -c "SETUP phase2 DataStream(130)" /tmp/airplayd_wl.log   # arrived AND reached its handler
      grep -nE "SETUP stream type=|NOT IMPLEMENTED|skipping" /tmp/airplayd_wl.log   # any refusal, whatever guard adds one next

  Non-zero on the first = the phone asked and we handled it. Zero on the first is **not** by itself proof
  the phone stayed silent — a guard can drop the SETUP with no log at all, which is how the 2026-08-10
  outage hid. Settle it from the request itself: `touch /tmp/setup_dump` (wireless arm, `av.rs`; the wired
  supervisor arms the same `CARPLAY_SETUP_DUMP` via `/tmp/logtransfer_test` or `/tmp/mainbuffered_test`),
  reconnect, and read `/tmp/setup_req.N` — each raw SETUP request plist is written before any guard runs.
  A `type 130` entry there with no `DataStream(130)` line in the log = **the box is refusing a stream the
  phone is asking for**, a box-side bug that nothing in `/info` will explain. Nothing in `/tmp/setup_req.*`
  and nothing in the log = the phone really never asked (then check `/info` + `enabledFeatures`).
- **Stream-130 SETUP arrives but is SKIPPED** → a SETUP-level guard is rejecting it before its handler
  runs. It legitimately carries no `streamConnectionID` (docs/carplay/05_METADATA_AND_CONTROLS.md §1.3), so any guard rejecting a zero
  or absent scid must exempt it. Symptom set: tunnel pinned at `state=Init`, zero `0x5001`, **A/V
  totally healthy**, and zero `iAPSendMessage` 400s — there are no sends to reject, so absence of
  errors here is not evidence of health. Regression-tested at
  `crates/vendor/receiver/tests/setup_stream_130.rs`; history in docs/carplay/05_METADATA_AND_CONTROLS.md §8.

### Session ordering

Do not disable Bluetooth — the WiFi handoff is delivered over the BT iAP2 link, so there is no session
without it. It is also unnecessary: the archive gives BT-Classic control rows **inside the same trace**
as the AirPlay rows — same phone, same daemon instance, same second, one variable.

Change nothing else this session. Eight fixes landed together, but seven are structural prerequisites
for reading the eighth cleanly; with the startup race live a negative result would be uninterpretable.

Near the end, trigger `modesChanged` (change audio app / start-stop navigation) to exercise the closed
re-DETECT routes for free. Expect `link already up (SYN-ACK received) — not re-DETECTing` or
`resent SYN only`. There is no `resent DETECT+SYN` string in the code — the re-DETECT routes were closed, so the
observable is the ABSENCE of a second `TX detect+SYN` line, not a dedicated warning.
