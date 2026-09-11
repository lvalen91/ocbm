# 2026-07-25 — inbound tunnel traffic arrives on the CONTROL connection (live capture)

Archived because `docs/carplay/03_SDK_GROUND_TRUTH.md` §1's central claim rests on it, and a review correctly flagged it as
unverifiable while it existed only in `/tmp/airplayd_wl.log` on the box (tmpfs — lost on reboot).

**Session:** the first successful wireless CarPlay session after the BT pairing fix (docs/wireless/01_BT_AND_RADIO.md). Box
`CarLink-b0df`, iPhone `64:31:35:8C:29:69`, HEVC video + audio streaming normally throughout
(`video ok=3964 fail=0, audio ok=5526 fail=0` at time of capture).

**Deployed binaries at capture time:** `airplayd 3ce47fbde4b0a1baadeb3fa36bad71a0`,
`ocbmd 52ae8bf981e09ebc33f3cdb73b8e4bba`, `iap2d 3a9eb1a1057702b07844231d729e6ad1`,
`carplay-wireless 6a9574bc8ffae0cf1cc621e9f316e80c`.

## The evidence

`grep -E "iPhone POST /command type=|iap-tunnel|^\[events\]" /tmp/airplayd_wl.log`:

```
[events] encrypted command channel ready
[iap-tunnel] TX detect+SYN over AirPlay tunnel (detect_sent=true syn_sent=true) — starting fresh iAP2 session
[command] ← iPhone POST /command type='modesChanged' (287 B)
[command] ← iPhone POST /command type='modesChanged' (287 B)
[command] ← iPhone POST /command type='disableBluetooth' (115 B)
[command] ← iPhone POST /command type='modesChanged' (287 B)
[command] ← iPhone POST /command type='modesChanged' (287 B)
[command] ← iPhone POST /command type='modesChanged' (287 B)
[command] ← iPhone POST /command type='modesChanged' (287 B)
[command] ← iPhone POST /command type='modesChanged' (287 B)
```

Counted totals for the session:

| Source | Count |
|---|---|
| `[command] ← iPhone POST /command type='modesChanged'` (CONTROL connection, `session.rs`) | **8** |
| `[command] ← iPhone POST /command type='disableBluetooth'` (CONTROL connection) | **1** |
| `POST /command` total (control) | **18** |
| `[events]` lines (EVENT channel, `events.rs`) | **1** — its own `encrypted command channel ready` startup line |
| Inbound iAP frames matched by the tunnel (`iAPSendMessage` / link frames) | **0** |
| `/tmp/carplay_event_capture.bin` size | **224 bytes** |

## What it establishes

1. **All phone→accessory commands arrived on the control connection.** The event channel received
   nothing inbound for the entire session. This matches Apple's R14G17 source, where the event socket's
   unsolicited-inbound mode (`kHTTPClientFlag_Events`) is never enabled — see docs/carplay/03_SDK_GROUND_TRUTH.md §1.
2. **The tunnel's DETECT+SYN was sent successfully and drew no reply.** At capture time the handshake
   machinery was wired only to the event channel, and the control-channel path routed iAP frames
   straight to the post-Identify metadata parser — so a `FF 5A` SYN-ACK arriving there would have been
   dropped as an unrecognised shape. Fixed in commit `bef6561` — **re-identified 2026-08-16 as `0eacf27`
   ("Tunnel inbound arrives on the CONTROL channel, not the event channel"); the SHA recorded here was
   orphaned by a history rewrite and is no longer a valid object in this repo.**
3. **The one-shot `modesChanged` tunnel nudge never fired** despite 8 `modesChanged`, because it also
   lived only on the event channel. Fixed in the same commit (shared atomic, called from both paths).

## What it does NOT establish

It does not show iOS ever *sending* an `iAPSendMessage` to us — zero arrived on either channel. So this
capture proves where inbound commands land and that our handshake could not have progressed; it does
**not** prove the phone would have answered a correctly-routed SYN. That remains open until a session
runs with those two fixes deployed to the box.

> **SHAs re-identified 2026-08-16 — this section could not be checked as written.** The commits this
> paragraph made the open question conditional on, `bef6561` and `cd1ac62`, are **not valid objects in
> this repo**: a history rewrite left `18ba44b` ("Baseline before 2026-07-25 QC remediation batch") as the
> earliest reachable commit and orphaned everything before it, so `git show` on either SHA fails. Both
> were recovered by commit message and content: `bef6561` → **`0eacf27`** ("Tunnel inbound arrives on the
> CONTROL channel, not the event channel") and `cd1ac62` → **`c4f90fa`** ("Zero-Ack link params for the
> tunnel, plus the /command reply body" — `link.rs: SYN_PARAMS_ZERO_ACK` plus the peer-disagreement
> fallback and the `/command` empty-dict bplist body; it is the commit `docs/carplay/03_SDK_GROUND_TRUTH.md` §2/§8 describes). Both
> are ancestors of `HEAD`, so the code is in the tree — but whether a wireless session has since been RUN
> on hardware with them deployed is not recorded here, so the question above stays OPEN, not answered.
> `docs/carplay/03_SDK_GROUND_TRUTH.md` still cites the dead `cd1ac62` in three places (`:87`, `:288`, `:310`).
