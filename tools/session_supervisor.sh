#!/bin/sh
# session_supervisor.sh — docs/carplay/02_SESSION_LIFECYCLE.md lifecycle state-machine ACTOR (box side). Launched at boot by
# ocbm_boot.sh and then IDLE-WAITS: post-boot the box does nothing to the phone until a HOST APP
# commands it. It reads ocbmd's presence signal /tmp/host_present and:
#
#   host PRESENT (1) -> go projection-ready + ARM:
#        projection_up.sh (iAP2 handshake -> Identified, if not already) THEN airplayd + rx-connect.
#   host GONE   (0) -> TEARDOWN: kill airplayd + rx-connect -> HOLDING PATTERN (iap2d stays up).
#
# ocbmd supplies the graces before it ever moves the flag: HEARTBEAT_GRACE=10s (beat loss) and
# STOP_GRACE=5s (clean CT_STOP), both in ccpa/ocbmd/src/main.rs. A blip inside either never reaches
# here. NOTE: this file defines no heartbeat constant of its own — it is edge-triggered on the flag.
#
# P0 lifecycle hardening (docs/carplay/02_SESSION_LIFECYCLE.md,§2 — tasks #22,#23): the supervisor no longer treats "airplayd
# process alive" as healthy. It derives a real HEALTH signal from establishment milestones in
# airplayd.log (pair-verify OK -> RECORD = Apple's "session established" edge), publishes it to
# /tmp/session_healthy, and tracks STUCK counters (establishment stalls + presence flaps) that SURVIVE
# teardown and reset only after a session has been confirmed-established for a hold period. This closes
# the docs/carplay/02_SESSION_LIFECYCLE.md stall, where "ARMED + airplayd-alive" masked a session that never streamed and the one
# counter was wiped by teardown() on every GONE edge. The phone_reset escalation LADDER that ACTS on
# health=STUCK lands in task #24; here we detect, publish, bound the retry, and log the verdict.
set -u
export PATH=/usr/sbin:/usr/bin:/sbin:/bin:$PATH
FLAG=${FLAG:-/tmp/host_present}
AL=/tmp/airplayd.log
RL=/tmp/rx-connect.log
HEALTHY=/tmp/session_healthy
STATE=/tmp/carplay_state          # canonical one-glance lifecycle verdict (task #26)
RING=/tmp/lifecycle.ndjson        # per-transition history ring, uptime-stamped (task #30)
PEER_PENDING=/tmp/peer_pending    # deferred peer-store mutation to apply at the idle boundary (#25)

# --- tunables (seconds / counts). Milestone-aware and deliberately generous so a slow cold pair-setup
# (the human "Allow CarPlay" tap can take tens of seconds, BEFORE pairing) is never falsely torn down.
# The fast presence-flap detector (below) catches the docs/carplay/02_SESSION_LIFECYCLE.md signature quickly regardless. See docs/carplay/02_SESSION_LIFECYCLE.md
# guardrail. Values are unvalidated against measured hardware latency — tune on the box.
ESTAB_CONNECT_GRACE=90   # ARMED -> must reach pair-verify within this (covers the human Allow tap)
ESTAB_STREAM_GRACE=30    # after pair-verify -> must reach RECORD within this (RECORD follows quickly)
CONFIRM_HOLD=15          # a session must stay established this long before STUCK counters are cleared
FLAP_N=5                 # presence 0->1 edges ...
FLAP_WINDOW=20           # ... within this window => flapping (the docs/carplay/02_SESSION_LIFECYCLE.md signature)
STALE_FLAG_N=3           # consecutive ticks (≈s) a "wireless" transport flag may persist with NO
                         # carplay-wireless AND no airplayd alive before the stale-flag watchdog clears
                         # it (dual-transport self-heal of a crashed/SIGKILLed wireless session)
# escalation ladder (task #24 / docs/carplay/02_SESSION_LIFECYCLE.md):
L1_AT=2                  # establishment stalls before the first phone_reset (L1)
L1_MAX=2                 # max L1 (phone_reset) attempts before escalating to L2
L2_AT=4                  # establishment stalls that force L2 regardless of L1 count
L2_MAX=2                 # max L2 (full daemon restart) before parking IDLE (L3 reboot = task #28)
PROJ_AT=3                # consecutive projection bring-up failures before escalating to L1 (phone_reset)
L3_MAX_REBOOTS=3         # consecutive L3 reboots without a good session before parking (persistent budget)
REBOOT_BUDGET=/etc/ccpa_reboot_count   # persists across reboots (jffs2); a plain count, no RTC needed

armed=0
fails=0                  # consecutive airplayd-death re-ARMs, for backoff
proj_fails=0             # consecutive projection_up.sh failures (pre-ARM), for the #24 ladder fix
backoff_until=0          # epoch(uptime) seconds; while now < this, do not re-ARM
tick=0
stale_flag_ticks=0      # consecutive ticks the transport flag looked orphaned (stale-flag watchdog, C)
last_p=""               # previous DEFINITE presence read, for 0->1 edge detection

# per-session establishment state (reset on ARM / TEARDOWN)
saw_paired=0
saw_record=0
healthy=0
estab_deadline=0
established_since=0

# cross-session STUCK state — INTENTIONALLY survives teardown(); reset only on a confirmed-established
# session (docs/carplay/02_SESSION_LIFECYCLE.md; fixes the teardown() counter-wipe that made the docs/carplay/02_SESSION_LIFECYCLE.md flap uncountable).
stuck=0
stuck_fails=0
edge_ts=""              # space-list of recent present 0->1 edge timestamps (uptime secs)
edge_count=0
l1_tries=0             # phone_reset (L1) attempts this stuck episode
l2_tries=0             # full-daemon-restart (L2) attempts this stuck episode
stuck_reason=""        # last STUCK reason string, surfaced in /tmp/carplay_state
last_state=""          # signature of the last /tmp/carplay_state write (change-detection)
last_phase=""          # last published phase, for the transition ring (#30)

now() { cut -d. -f1 /proc/uptime; }

PHONE_FLAG=/tmp/phone_present     # phone-on-bus signal for ocbmd -> host SEV_PHONE_* (2026-07-12)
phone_flag_state=""
phone_waiting=0

# Is a GENUINE wired iPhone physically enumerated on the USB bus right now — Apple VID 05ac, NORMAL mode
# (before any role switch)? This is the raw bus probe projection_up.sh relies on (projection_up.sh:24),
# expressed against sysfs (the same 05ac idVendor match phone_on_bus has always used), and it is
# INDEPENDENT of /tmp/carplay_transport — so the preempt can see a real cable even while a stale flag
# still says "wireless". A wireless phone is on Wi-Fi and never on this bus, so this is ALWAYS false for a
# wireless-only session (the safety property preempt_wireless_for_wired relies on). Note: once a phone
# role-switches into projection it stops enumerating as 05ac (projection_up.sh:17), so this matches a
# FRESHLY-plugged cable — exactly the preempt trigger — not an already-projected wired session.
wired_iphone_on_usb() {
  grep -q 05ac /sys/bus/usb/devices/*/idVendor 2>/dev/null
}

# Is a non-Apple phone (an Android Auto candidate) on the host-facing bus? Any USB idVendor that is
# neither Apple (05ac) nor a Linux-Foundation root hub/controller (1d6b) — this covers a normal-mode
# Android phone AND its post-AOAP accessory re-enumeration (0x18d1:0x2d0x), so it stays true across the
# switch. Mirrors box_common::phone::classify; the DEFINITIVE Android-Auto test is aa-bridge's AOAP
# getProtocol probe, so this only has to be a cheap selection hint (docs/host/02_ANDROID_AUTO.mdb).
android_phone_on_bus() {
  for _v in /sys/bus/usb/devices/*/idVendor; do
    [ -f "$_v" ] || continue
    case "$(cat "$_v" 2>/dev/null)" in
      05ac | 1d6b | "") continue ;;
    esac
    # A HUB is not a phone. bDeviceClass 0x09 is mandatory in every hub's device descriptor
    # (USB 2.0 §11.23.1) and is never used by a phone — Apple, Android normal mode and AOAP
    # accessories are all per-interface (0x00). Without this an external hub (or a dashcam, or a card
    # reader) reads as an Android Auto candidate, which launches a bridge that AOAP-probes it forever
    # and suppresses wireless CarPlay for anyone using one. Mirrors box_common::phone::classify_dev.
    case "$(cat "${_v%/idVendor}/bDeviceClass" 2>/dev/null)" in
      09 | 9) continue ;;
    esac
    return 0
  done
  return 1
}

# Is an iPhone attached to the phone-facing port? TWO modes (the role switch flips them):
#   normal mode  — the iPhone is a 05ac USB DEVICE on the box's host controller (sysfs idVendor);
#   projected    — after the 0x51 role switch the iPhone is the USB HOST and the box is a gadget,
#                  so it VANISHES from the device list; presence = the phone-facing gadget
#                  (/sys/class/android_usb/android0, ci_hdrc.0) being CONFIGURED by it.
# Checking only the device list caused the 2026-07-12 deadlock: a projected phone read as "absent",
# the app showed a false "waiting for phone", and after a stall the gate blocked re-arm forever.
phone_on_bus() {
  # (B) Check the RAW USB bus for a genuine wired iPhone FIRST, so a live-wireless flag can never MASK a
  # real cable. This ordering is what unblinds the wired preempt: previously the `wireless_owns_session`
  # short-circuit below returned before the 05ac check ever ran, so a wired iPhone plugged in during (or
  # after a stale) wireless was structurally invisible. Flag-independent, so it still detects the cable
  # while the flag says "wireless".
  wired_iphone_on_usb && return 0
  # A live WIRELESS session means the phone IS present — just over Wi-Fi, not USB (#87). Without this,
  # the USB checks report "phone absent" during a wireless session, which ocbmd relays to the host app as
  # SEV_PHONE_ABSENT — making the app's watchdog blank the video / say "waiting for phone" on a perfectly
  # live wireless screen. (Kept below the wired check so it can no longer mask a real cable.)
  wireless_owns_session && return 0
  [ "$(cat /sys/class/android_usb/android0/state 2>/dev/null)" = "CONFIGURED" ]
}

# Publish the phone-presence flag atomically on transitions (ocbmd mirrors it to the host app so
# "waiting for phone" is truthful and immediate, not a 20 s watchdog).
write_phone_flag() {  # $1 = 0|1
  [ "$1" = "$phone_flag_state" ] && return
  phone_flag_state="$1"
  printf '%s' "$1" > "$PHONE_FLAG.tmp" 2>/dev/null && mv "$PHONE_FLAG.tmp" "$PHONE_FLAG" 2>/dev/null
}

write_healthy() {  # $1 = 0|1 — atomic publish of the health signal for other consumers
  printf '%s' "$1" > "$HEALTHY.tmp" 2>/dev/null && mv "$HEALTHY.tmp" "$HEALTHY" 2>/dev/null
}

# write_state — publish the canonical one-glance lifecycle verdict /tmp/carplay_state (task #26).
# Edge-triggered: only rewrites when the meaningful signature changes (the uptime is excluded from the
# signature so a healthy session doesn't churn the file every tick), so overhead is a few writes per
# transition on the tmpfs, never per frame/tick. Read it with carplay-status.
write_state() {   # $1 = phase, $2 = reason
  _sig="$1|$armed|$healthy|$stuck|$saw_paired|$saw_record|$edge_count|$stuck_fails|$l1_tries|$l2_tries|$fails|${last_p:-?}|$2"
  [ "$_sig" = "$last_state" ] && return
  last_state="$_sig"
  {
    echo "phase=$1"
    echo "host_present=${last_p:-?}"
    echo "armed=$armed healthy=$healthy stuck=$stuck"
    echo "paired=$saw_paired record=$saw_record"
    echo "flaps=$edge_count stuck_fails=$stuck_fails l1=$l1_tries l2=$l2_tries fails=$fails"
    echo "reason=$2"
    echo "uptime=$(now)"
  } > "$STATE.tmp" 2>/dev/null && mv "$STATE.tmp" "$STATE" 2>/dev/null
}

# Kill any running session daemons (idempotent). iap2d/iAP2 are deliberately left up (holding pattern).
# True while a live WIRELESS session owns airplayd (airplayd writes /tmp/carplay_transport=wireless on
# its control connection). The wired supervisor must NOT touch airplayd/rx-connect in that case — the
# critical QC finding: the old unconditional kill SIGKILLed a healthy wireless projection the moment a
# USB phone appeared, and the establishment-stall ladder then escalated to REBOOTING the box at it.
wireless_owns_session() {
  [ "$(cat /tmp/carplay_transport 2>/dev/null)" = "wireless" ]
}

# The single projection OWNER across all transports (box_common::flags model, /tmp/projection_owner).
# Prefers the unified flag and falls back to the legacy wireless-only /tmp/carplay_transport, so it is
# byte-compatible with the existing wireless arbitration above. A 'wired-aa' owner is LIVENESS-checked:
# if the flag claims AA owns the box but aa-bridge is gone (unclean exit), the flag is stale — clear it
# and fall through, so a crashed Android Auto session can never wedge the CarPlay path. (docs/host/02_ANDROID_AUTO.mda.)
# Claim/release the unified owner flag for a WIRED CARPLAY session.
#
# Until now nothing ever wrote `wired-cp`, so `projection_owner()` could not tell "idle" from "CarPlay
# is streaming" — and aa-bridge, which reads the same flag, could re-claim the box out from under a
# live CarPlay session. Its own stand-downs do not cover that: `carplay_session_live` gates only the
# LAUNCH of a bridge (a resident one re-claims without passing it again), and the bridge's
# `apple_on_bus` check goes blind the moment the iPhone role-switches out of 05ac. Writing the flag
# gives the bridge a signal that stays true for the whole session.
claim_carplay_owner() { echo wired-cp > /tmp/projection_owner 2>/dev/null; }
# Only ever clears OUR token, so it can't stomp a wireless or AA claim.
release_carplay_owner() {
  [ "$(cat /tmp/projection_owner 2>/dev/null)" = "wired-cp" ] && rm -f /tmp/projection_owner 2>/dev/null
  return 0
}

projection_owner() {
  o=$(cat /tmp/projection_owner 2>/dev/null)
  case "$o" in
    wired-cp)
      # Same liveness self-heal the wired-aa arm gets: if the flag claims CarPlay owns the box but its
      # daemons are gone (crash, missed release), the claim is stale — clear it and fall through, so a
      # dead CarPlay session can never lock Android Auto out permanently.
      #
      # `iap2d` ALONE, not `airplayd || iap2d` (F2, device-proven 2026-08-27). WIRED CarPlay IS the
      # Identified iAP2 link, and that link is iap2d; airplayd is NOT evidence of one. This very
      # function's caller deliberately leaves airplayd running when the host goes away
      # ("airplayd/rx-connect are wired-owned, left running"), and airplayd is shared with the
      # wireless arm — so an `||` here means a surviving airplayd keeps a dead session's `wired-cp`
      # claim alive FOREVER. Measured: unplug the iPhone mid-session -> iap2d exits, airplayd stays,
      # flag stays `wired-cp` with an EMPTY bus; the box then reported "wired CarPlay" to the app with
      # nothing plugged in, and plugging an Android phone in did nothing at all because arm_aa stands
      # down against the flag. That is exactly the iPhone->Android swap failure.
      if pgrep iap2d >/dev/null 2>&1; then echo wired-cp; return; fi
      rm -f /tmp/projection_owner 2>/dev/null
      ;;
    wired-aa)
      # NAME-based match (comm), not `pgrep -f` — a full-cmdline match false-positives on any process
      # whose args merely mention aa-bridge (the supervisor, a run_aa_bridge.sh wrapper, a debug shell).
      if pgrep aa-bridge >/dev/null 2>&1; then echo wired-aa; return; fi
      rm -f /tmp/projection_owner 2>/dev/null   # stale (aa-bridge gone) — self-heal
      ;;
    wireless) echo "$o"; return ;;
  esac
  [ "$(cat /tmp/carplay_transport 2>/dev/null)" = "wireless" ] && { echo wireless; return; }
  echo ""
}

# True while a live wired Android Auto session (aa-bridge) owns the phone-facing controller ci_hdrc.0.
# The wired-CarPlay supervisor must NOT run projection_up / kill_session / escalate (which includes
# phone_reset.sh's real USB port reset) against it — the same doctrine as wireless_owns_session,
# extended to Android Auto. Without this guard, a spurious SUBSCRIBE during an AA session could burn
# the PROJ_AT ladder and reset ci_hdrc.0 out from under a live AOAP link (docs/host/02_ANDROID_AUTO.md).
aa_owns_session() {
  [ "$(projection_owner)" = "wired-aa" ]
}

kill_session() {
  if aa_owns_session; then
    echo "[sup] kill_session suppressed — a live Android Auto session owns the phone port"
    return 0
  fi
  if wireless_owns_session; then
    echo "[sup] kill_session suppressed — a live wireless session owns airplayd"
    return 0
  fi
  pkill -f airplayd 2>/dev/null
  pkill -f rx-connect 2>/dev/null
  # brief SIGKILL fallback so a stuck airplayd can't double-bind :5000 on the next ARM
  sleep 1
  pkill -9 -f airplayd 2>/dev/null
  pkill -9 -f rx-connect 2>/dev/null
  release_carplay_owner
}

# (A) Preempt a wireless-owned session when a GENUINE wired iPhone is physically on the USB bus. Called
# ONLY from the main loop's `wireless_owns_session && wired_iphone_on_usb` guard, so it can NEVER fire
# during a healthy wireless-ONLY session: a wireless phone is on Wi-Fi, not the USB bus, so
# wired_iphone_on_usb() (a raw 05ac sysfs probe, flag-independent) is false for it. SIGTERM
# carplay-wireless so its own teardown_av_layer() (av.rs) clears the transport flag + AV_LAYER_UP latch
# and reaps the wireless airplayd/rx-connect, then WAIT (bounded ~5s) for the flag to actually clear
# before returning, so the caller's arm() takes the wired path cleanly. If carplay-wireless is already
# gone (a stale flag with no owner), clear the flag directly so the wired takeover is never blocked.
preempt_wireless_for_wired() {
  echo "[sup] PREEMPT: genuine wired iPhone on USB while transport=wireless -> SIGTERM carplay-wireless, switching to wired"
  pkill -f /usr/sbin/carplay-wireless 2>/dev/null
  _i=0
  while [ "$_i" -lt 25 ]; do
    wireless_owns_session || { echo "[sup] PREEMPT: transport flag cleared -- proceeding to wired arm"; return 0; }
    _i=$((_i + 1)); sleep 0.2
  done
  echo "[sup] PREEMPT: flag still 'wireless' after ~5s wait -- clearing stale flag directly so wired can arm"
  rm -f /tmp/carplay_transport
}

# Gated launch of the Android Auto USB bridge (the AA analogue of arm()). aa-bridge is long-lived: it
# listens for the macOS host app, runs the AOAP switch on the Android phone, pumps the raw AA stream,
# and claims /tmp/projection_owner=wired-aa on an active session (step 1). Idempotent — a bridge is
# already running is a no-op; the supervisor loop calls this to (re)launch after a crash while the
# Android phone is still present. Not an inittab respawn: launched ONLY here, gated on AA selection.
AA_BRIDGE=${AA_BRIDGE_BIN:-/usr/sbin/aa-bridge}
arm_aa() {
  if pgrep aa-bridge >/dev/null 2>&1; then
    return 0
  fi
  if [ ! -x "$AA_BRIDGE" ]; then
    echo "[sup] arm_aa: $AA_BRIDGE not installed — Android Auto unavailable"
    return 1
  fi
  echo "[sup] Android phone on bus + AA enabled -> launching aa-bridge (Android Auto)"
  setsid "$AA_BRIDGE" >> /tmp/aa-bridge.log 2>&1 &
}

arm() {
  if aa_owns_session; then
    echo "[sup] ARM suppressed — a live Android Auto session owns the phone port (dual-mode, first-come-wins)"
    return 1
  fi
  if wireless_owns_session; then
    echo "[sup] ARM suppressed — a live wireless session owns airplayd (dual-transport, first-come-wins)"
    return 1
  fi
  echo "[sup] host PRESENT -> go projection-ready + ARM"
  /script/projection_up.sh; _pu=$?
  if [ "$_pu" = 2 ]; then
    # Exit 2 = the BOX is missing something (projection_up's prerequisite check), not a phone fault.
    # Propagated so the caller can refuse to count it toward the ladder — see the PROJ_ENV branch.
    echo "[sup] projection bring-up failed on a BOX MISCONFIGURATION — not arming, not escalating"
    return 2
  fi
  if [ "$_pu" != 0 ]; then
    echo "[sup] projection bring-up failed — not arming (will retry while present)"
    return 1
  fi
  kill_session   # idempotent: never leak a duplicate airplayd/rx-connect on re-ARM
  : > "$AL"; : > "$RL"
  # docs/wireless/00_WIRELESS_CARPLAY.md: the wireless-metadata experiment (CARPLAY_WIRELESS_METADATA) is set ONLY at the wireless
  # spawn site (crates/vendor/wireless/src/av.rs::ensure_av_layer) — this is the WIRED launcher, and it
  # deliberately does NOT set that var: info.rs/session.rs's iAPChannelInfo/enabledFeatures echoes it
  # gates are NOT wireless-scoped (only events.rs's actual tunnel send is), so setting it here would
  # silently change the proven wired session's /info + SETUP-response bytes with zero live wired testing.
  # cornerMask experiment (Phase 1): OPT-IN via the on-box flag file /tmp/cornermask_test (tmpfs, so a
  # reboot disarms it — mirrors the /tmp/carplay_metadata lever philosophy). When present, arm the
  # SETUP `enabledFeatures`/viewAreas `cornerMasks` advertisement (CARPLAY_CORNERMASKS) and dump any
  # `topLeftCornerMask` the phone streams (CARPLAY_CORNERMASK_CAPTURE=/tmp). Absent = byte-identical to
  # the proven wired spawn. Re-arm after a reboot with: echo 1 > /tmp/cornermask_test
  CM=
  [ -f /tmp/cornermask_test ] && CM="CARPLAY_CORNERMASKS=1 CARPLAY_CORNERMASK_CAPTURE=/tmp"
  # logTransfer Tier-1 experiment (docs/carplay/04_CAPABILITIES_AND_CONFIG.md Half A): OPT-IN via /tmp/logtransfer_test, same tmpfs
  # philosophy as the cornermask flag above. Arms `logTransferInfo` in /info + the `logTransfer`
  # enabledFeatures echo. Absent = byte-identical to the proven wired spawn.
  LT=
  # SETUP_DUMP rides the same flag: each raw SETUP request plist lands at /tmp/setup_req.N, pinning
  # the feature-token array the iPhone actually proposes (the enabledFeatures intersection input —
  # a token we advertise but the phone never proposes can never appear negotiated; docs/carplay/04_CAPABILITIES_AND_CONFIG.md).
  [ -f /tmp/logtransfer_test ] && LT="CARPLAY_LOGTRANSFER=1 CARPLAY_SETUP_DUMP=/tmp/setup_req"
  # mainBufferedAudio Phase-A (docs/carplay/04_CAPABILITIES_AND_CONFIG.md; docs/carplay/04_CAPABILITIES_AND_CONFIG.md B4): the PRIMARY arm is now the pushed config's
  # enablesMainBufferedAudio (app default OFF, applied per connection by airplayd) — this
  # /tmp/mainbuffered_test flag is SUBORDINATE and reaches the wire only on the no-config /
  # parse-failure paths (it seeds the lever via the env; ANY parsed pushed config overwrites it in
  # both directions, so with the app connected it is inert). CARPLAY_SETUP_DUMP rides the flag so
  # the phone's SETUP request (does iOS request a buffered stream / move media off the realtime
  # stream?) is CAPTURED, not inferred. Absent = byte-identical to the proven wired spawn.
  # WARNING: advertising can silence media if iOS switches to a buffered stream we do not serve —
  # bench flag OR config toggle, remove/disable immediately on any audio dropout.
  MB=
  [ -f /tmp/mainbuffered_test ] && MB="CARPLAY_MAINBUFFERED=1 CARPLAY_SETUP_DUMP=/tmp/setup_req"
  env OCBM_FWD_ENC=1 $CM $LT $MB setsid airplayd >>"$AL" 2>&1 &   # /usr/sbin/airplayd
  setsid rx-connect >>"$RL" 2>&1 &                # /usr/sbin/rx-connect
  armed=1
  claim_carplay_owner   # tell the box (and aa-bridge) that wired CarPlay owns the port
  # fresh establishment window for this session
  saw_paired=0; saw_record=0; healthy=0; established_since=0
  estab_deadline=$(( $(now) + ESTAB_CONNECT_GRACE ))
  write_healthy 0
  echo "[sup] ARMED (airplayd + rx-connect) — awaiting pair-verify -> RECORD"
  return 0
}

teardown() {
  echo "[sup] host GONE -> TEARDOWN (holding pattern; iap2d/iAP2 stay up)"
  kill_session
  armed=0
  # per-session establishment state resets; the cross-session STUCK counters (stuck/stuck_fails/edge_ts/
  # fails) DELIBERATELY survive — they clear only on a confirmed-established session (docs/carplay/02_SESSION_LIFECYCLE.md). The
  # old `fails=0` here was the docs/carplay/02_SESSION_LIFECYCLE.md bug: every GONE edge wiped the only counter, so the flap was
  # never counted.
  saw_paired=0; saw_record=0; healthy=0; established_since=0
  write_healthy 0
}

# escalate — the recovery LADDER (task #24 / docs/carplay/02_SESSION_LIFECYCLE.md). Picks a rung by how many stalls/resets have
# accumulated this stuck episode; the counters survive teardown and clear only on a confirmed-established
# session (see the reset in the loop). L3 (reboot, under a persistent budget) is task #28.
escalate() {   # $1 = reason
  # TEST INHIBIT: while /tmp/no_escalate exists, the whole ladder (phone_reset -> ocbmd restart ->
  # REBOOT) is disabled. Bring-up testing produces exactly the signals the ladder is built to punish
  # — repeated app reconnects and sessions that never reach RECORD — and a reboot mid-experiment
  # destroys the state being measured. tmpfs, so it clears itself on the next boot.
  if [ -f /tmp/no_escalate ]; then
    echo "[sup] escalate INHIBITED ($1) — /tmp/no_escalate present (testing)"
    armed=0
    return
  fi
  # NEVER escalate (least of all REBOOT / phone_reset.sh USB port reset) while a live Android Auto
  # session owns ci_hdrc.0 — the wired-CarPlay watchdog must not reset the controller under a live
  # AOAP link it doesn't manage (docs/host/02_ANDROID_AUTO.md).
  if aa_owns_session; then
    echo "[sup] escalate suppressed ($1) — a live Android Auto session owns the phone port"
    armed=0
    return
  fi
  # NEVER escalate (least of all REBOOT) while a live wireless session owns airplayd — the wired
  # establishment watchdog must not act on a wireless session it doesn't manage (critical QC finding).
  if wireless_owns_session; then
    echo "[sup] escalate suppressed ($1) — a live wireless session owns airplayd"
    armed=0
    return
  fi
  if [ "$l2_tries" -ge "$L2_MAX" ]; then
    # L1 + L2 exhausted -> L3: reboot — the ONLY proven deep-wedge recovery (see the 2026-07-10 USB-reset
    # investigation: no VBUS/controller/PHY reset re-enumerates a wedged iPhone). Gated by a PERSISTENT
    # consecutive-reboot budget so a permanently-broken box can't reboot-loop; the count clears on a
    # confirmed-good session (below). No RTC needed — a plain count in jffs2.
    rc=$(cat "$REBOOT_BUDGET" 2>/dev/null || echo 0)
    case "$rc" in ''|*[!0-9]*) rc=0 ;; esac
    if [ "$rc" -lt "$L3_MAX_REBOOTS" ]; then
      # FAIL CLOSED (audit N4): the budget is the ONLY thing bounding L3, and root jffs2 runs near
      # full, so an unchecked write here can silently leave the count unchanged -> unbounded reboots.
      # Read it back after sync and reboot only if it actually persisted; otherwise fall through to
      # the park-IDLE branch below. A reboot into a still-full filesystem is a loop, not a recovery.
      echo $((rc + 1)) > "$REBOOT_BUDGET" 2>/dev/null; sync
      if [ "$(cat "$REBOOT_BUDGET" 2>/dev/null)" = "$((rc + 1))" ]; then
        echo "[sup] health=STUCK reason=$1: L1/L2 exhausted -> ESCALATE L3 REBOOT (#$((rc + 1))/$L3_MAX_REBOOTS)"
        write_healthy 0; sync; reboot
        return
      fi
      echo "[sup] CRITICAL: reboot budget did not persist to $REBOOT_BUDGET (filesystem full?) — refusing L3 REBOOT"
    fi
    [ "$stuck" = 0 ] && echo "[sup] health=STUCK reason=$1: L3 reboot budget exhausted ($rc) — parking IDLE (needs manual intervention)"
    stuck=1; stuck_reason="$1"; write_healthy 0
    kill_session; armed=0
    backoff_until=$(( $(now) + 120 ))
    return
  fi
  if [ "$l1_tries" -lt "$L1_MAX" ] && [ "$stuck_fails" -lt "$L2_AT" ]; then
    # L1 — clean the phone-facing side (the automated power-cycle equivalent) and re-ARM.
    l1_tries=$((l1_tries + 1))
    echo "[sup] ESCALATE L1 (#$l1_tries): phone_reset then re-ARM ($1)"
    kill_session
    /script/phone_reset.sh >> /tmp/supervisor.log 2>&1
    armed=0; write_healthy 0
    backoff_until=$(( $(now) + 3 ))
  else
    # L2 — software power-cycle short of reboot: phone_reset + restart the OCBM control plane. The host
    # app must re-SUBSCRIBE afterwards. ocbmd restart is verified; if it fails to come back, only a
    # reboot recovers (task #28 adds the inittab respawn safety net + the L3 reboot rung).
    l2_tries=$((l2_tries + 1))
    echo "[sup] ESCALATE L2 (#$l2_tries): phone_reset + restart ocbmd ($1)"
    kill_session
    /script/phone_reset.sh >> /tmp/supervisor.log 2>&1
    # -x, not -f (audit N3), and BOTH spawn forms — exactly the `airplayd_alive` idiom below. `-f`
    # also matches the inittab RESPAWN WRAPPER (`{run_ocbmd.sh} /bin/sh /script/run_ocbmd.sh`):
    # killing it decapitates the thing that would bring ocbmd back, and matching it in the check
    # below certifies a DEAD daemon as alive.
    #
    # BOTH forms are load-bearing, NOT belt-and-braces: BusyBox 1.37 `pgrep/pkill -x` match against
    # the full `argv[0]`, not the basename (hardware-confirmed, see `wireless/src/av.rs`'s `running`).
    # Every real launch of this daemon uses the full path — inittab's `exec /usr/sbin/ocbmd` and
    # ocbm_boot.sh's `/usr/sbin/ocbmd &` — so a bare `-x ocbmd` alone would match NEITHER: the kill
    # would spare the live daemon and the relaunch below would make it a SECOND one, and the success
    # check would report CRITICAL against a daemon that had just started fine.
    pkill -x /usr/sbin/ocbmd 2>/dev/null; pkill -x ocbmd 2>/dev/null
    sleep 1
    pkill -9 -x /usr/sbin/ocbmd 2>/dev/null; pkill -9 -x ocbmd 2>/dev/null
    # Full path, matching inittab: the respawn wrapper polls `pgrep -f /usr/sbin/ocbmd` and is blind
    # to a bare-name `setsid ocbmd`, so the old spelling had it start a second daemon ~5 s later.
    setsid /usr/sbin/ocbmd >> /tmp/ocbmd.log 2>&1 &
    sleep 2
    if pgrep -x ocbmd >/dev/null 2>&1 || pgrep -x /usr/sbin/ocbmd >/dev/null 2>&1; then
      echo "[sup] L2: ocbmd restarted — host must re-SUBSCRIBE"
    else
      echo "[sup] CRITICAL: ocbmd did not restart — reboot required (L3 = task #28)"
    fi
    armed=0; last_p=""; edge_ts=""; stuck_fails=0   # fresh episode after the big hammer
    write_healthy 0
    backoff_until=$(( $(now) + 8 ))
  fi
}

# apply_pending — at an idle boundary (present==0), apply any peer-store mutation that peer_store.sh
# deferred while a host was present (docs/carplay/02_SESSION_LIFECYCLE.md, #25). Runs restart-coupled: the store is only ever
# mutated while idle, and the next ARM's airplayd re-reads it — never the live disk<->memory divergence
# that caused the docs/carplay/02_SESSION_LIFECYCLE.md stall. Safe to call every idle tick (no-op without a pending file).
apply_pending() {
  [ -f "$PEER_PENDING" ] || return
  op=$(cat "$PEER_PENDING" 2>/dev/null)
  case "$op" in
    clear) rm -f /etc/carplay_peers.bin 2>/dev/null; sync
           echo "[sup] applied DEFERRED peer-store op at idle: clear (cold pair-setup next connect)" ;;
    *)     echo "[sup] unknown deferred peer-store op '$op' — ignored" ;;
  esac
  rm -f "$PEER_PENDING"
}

# Latch establishment milestones (grep-once; bound_logs may truncate the line later, so never rely on
# re-grepping — once latched it stays latched for this session). docs/wireless/00_WIRELESS_CARPLAY.md #1.3: transport-scoped — while
# a wireless session owns the box, read ITS log (/tmp/airplayd_wl.log), never the wired $AL. Grepping
# both unconditionally would let a stale, never-truncated wl.log falsely mark an unrelated WIRED session
# healthy (the exact cross-attribution bug per-transport logs were created to prevent).
scan_milestones() {
  if wireless_owns_session; then _ml=/tmp/airplayd_wl.log; else _ml="$AL"; fi
  if [ "$saw_paired" = 0 ] && grep -q "pair-verify OK" "$_ml" 2>/dev/null; then
    saw_paired=1
    estab_deadline=$(( $(now) + ESTAB_STREAM_GRACE ))
    echo "[sup] milestone: pair-verify OK — control encrypted (RECORD grace ${ESTAB_STREAM_GRACE}s)"
  fi
  if [ "$saw_record" = 0 ] && grep -q "RECORD done" "$_ml" 2>/dev/null; then
    saw_record=1; healthy=1; established_since=$(now); write_healthy 1
    echo "[sup] milestone: RECORD — session ESTABLISHED (health=1)"
  fi
}

# Prune edge_ts to the flap window; set edge_count.
prune_edges() {
  _cut=$(( $(now) - FLAP_WINDOW )); _out=""; edge_count=0
  for _t in $edge_ts; do
    if [ "$_t" -ge "$_cut" ]; then _out="$_out $_t"; edge_count=$((edge_count + 1)); fi
  done
  edge_ts="$_out"
}

# Keep the RAM-backed /tmp logs bounded on the 123 MB no-swap box (belt-and-suspenders; the per-frame
# churn was already removed). Best-effort in-place truncate to the tail when a log exceeds the cap.
bound_logs() {
  # docs/wireless/00_WIRELESS_CARPLAY.md #1.5: the wireless-side logs (av.rs's spawn_detached targets + carplay-wireless's own log)
  # were omitted here — unbounded append on the 123 MB no-swap tmpfs across long-lived wireless sessions.
  # /tmp/aa-bridge.log joined the list 2026-08-25: arm_aa appends every (re)launch to it, and the
  # bridge now retries the AOAP switch on a backoff rather than staying inert, so a phone that never
  # completes the switch (MDM-blocked, charge-only) appends indefinitely.
  for f in /tmp/ocbmd.log /tmp/iap2d.log /tmp/supervisor.log "$AL" "$RL" \
           /tmp/aa-bridge.log \
           /tmp/airplayd_wl.log /tmp/rx-connect_wl.log /tmp/wl.log; do
    [ -f "$f" ] || continue
    sz=$(wc -c < "$f" 2>/dev/null || echo 0)
    if [ "$sz" -gt 262144 ]; then
      tail -c 65536 "$f" > "$f.lr" 2>/dev/null && cat "$f.lr" > "$f" 2>/dev/null
      rm -f "$f.lr"
    fi
  done
  # transition ring: count-bounded (keep last 200 once it passes 400), NOT byte-tail — transitions are
  # rare so this stays tiny, and the flap ONSET (the diagnostic gold) is never sliced off mid-history.
  if [ -f "$RING" ]; then
    lc=$(wc -l < "$RING" 2>/dev/null || echo 0)
    if [ "$lc" -gt 400 ]; then tail -n 200 "$RING" > "$RING.lr" 2>/dev/null && cat "$RING.lr" > "$RING" 2>/dev/null; rm -f "$RING.lr"; fi
  fi
}

# --- App-driven wireless bring-up (2026-07-16) -------------------------------------------------------
# Wireless CarPlay radios are ON-DEMAND, driven by HOST-APP presence: brought up when the app is present
# AND its pushed config enables wireless (default true), torn down when the host goes away. WIRED is the
# unconditional always-on baseline (ocbmd + this supervisor spawn iap2d/airplayd on iPhone-USB-connect
# regardless) — this only ADDS the wireless option alongside it, so the box waits for whichever transport
# connects FIRST (dual-transport, first-come-wins; the wireless_owns_session guards above keep them from
# fighting). The heavy bring-up (~15 s: WiFi/BT module loads) runs DETACHED so it never blocks the 1 s
# loop, and each stage is internally idempotent (wlan_on/bt_on skip if already loaded; carplay-wireless
# is pgrep-guarded). NEVER pipe a radio bring-up script to tail/head — a backgrounding daemon inheriting
# the console PTY wedges it; always redirect to a logfile.
WIRELESS_CFG=${CARPLAY_CFG_FILE:-/tmp/carplay_cfg.yaml}
# Default TRUE: only an explicit `wireless: false` in the host YAML disables it (wired-only). A missing
# file / missing key / `wireless: true` all mean enabled. Kept a raw grep so ocbmd + this shell need no
# YAML parser (airplayd's receiver ignores the key; the supervisor is its sole consumer).
wireless_enabled() { ! grep -qiE '^[[:space:]]*wireless:[[:space:]]*false' "$WIRELESS_CFG" 2>/dev/null; }
# Android Auto enable lever (app-driven, docs/carplay/04_CAPABILITIES_AND_CONFIG.md). Default ON so a plugged Android phone projects out
# of the box; the app opts out with `android_auto: false` in the pushed carplay_cfg.yaml (docs/host/02_ANDROID_AUTO.mde).
# This grep gates the LAUNCH only. An already-running aa-bridge reads the same key itself
# (box_common::cfg::aa_enabled — the Rust-side definition this must stay in step with) and stands
# down on it at every claim point AND mid-session, so turning the toggle off ends a live AA session
# rather than waiting for the phone to be unplugged (docs/host/02_ANDROID_AUTO.md F3).
aa_enabled() { ! grep -qiE '^[[:space:]]*android_auto:[[:space:]]*false' "$WIRELESS_CFG" 2>/dev/null; }

# Is a wired CarPlay session LIVE right now? Blocks arm_aa, because Android Auto must never be armed
# on top of a running CarPlay session — aa-bridge would claim /tmp/projection_owner out from under it,
# after which this supervisor treats the still-running CarPlay stack as "handed off" (no kill_session,
# no escalate) and the app is told to switch its window to AA. The session would be orphaned invisibly.
#
# `wired_iphone_on_usb` alone CANNOT carry this gate: it is the raw 05ac probe, and a phone that has
# already role-switched into projection no longer enumerates as 05ac (see its own comment above), so it
# reads FALSE for exactly the live session we must protect. `phone_on_bus` is no good either — its
# android0 CONFIGURED arm describes the HEAD-UNIT-facing gadget, so in OCBM mode it is true merely
# because the host app is attached. So test the session directly: this supervisor's own armed state,
# plus the wired CarPlay daemons it spawns.
carplay_session_live() {
  [ "${armed:-0}" = 1 ] && return 0
  # iap2d ONLY — a bare `airplayd` is NOT a live wired CarPlay session (F2, device-proven
  # 2026-08-27, the same mistake as the `wired-cp` self-heal in projection_owner()).
  #
  # A wired CarPlay session IS the Identified iAP2 link, and that link is iap2d: it exits the moment
  # the iPhone leaves the bus. airplayd does not — this script deliberately leaves it running when
  # the host goes away ("airplayd/rx-connect are wired-owned, left running") and the wireless arm
  # shares it — so testing airplayd here means ONE wired CarPlay session poisons the box for every
  # Android phone that follows it, until something kills airplayd.
  #
  # Measured before this change: iPhone streams, unplug it, plug in a Pixel -> the supervisor logs
  # "no iPhone (05ac) on the bus" and latches to WAITING, and `arm_aa` is never reached at all
  # because this returned true on the surviving airplayd. Android Auto simply never started.
  #
  # The AA-hijack protection this guard exists for is unaffected: a live wired CarPlay session always
  # has iap2d holding the link, so the case it must refuse still reads true.
  pgrep iap2d >/dev/null 2>&1 && return 0
  return 1
}
# App-commanded radio inhibit (ocbmd CT_RADIO, docs/carplay/04_CAPABILITIES_AND_CONFIG.md radio gating): flag present = radios must be
# OFF now. ocbmd owns the flag lifecycle (set/cleared by host CT_RADIO; cleared on fresh SUBSCRIBE,
# app loss, and ocbmd startup) — this is an app-commanded surface, NOT an on-box lever.
radio_off() { [ -f /tmp/radio_off ]; }
# Hot-Handover (NON-STANDARD, opt-in): force a live wireless->wired switch when a cable is plugged into an
# ACTIVE wireless session. DEFAULT OFF (= "Standard", spec-conformant): Apple's R14G17 selects transport
# exactly ONCE at session start (wired-preferred) and never migrates a live session (CarPlayControlClient.c
# _CarPlayControllerCopyBestService); iOS keeps the running wireless session and treats the cable as
# charge-only ("Charge Only" USB mode; WWDC 2017-717 "do not interrupt a running session"). So this preempt
# is a deliberate extension, enabled ONLY by the host YAML `hot_handover: true`. Absent/anything-else => the
# standard "keep wireless on plug" behavior. See docs/ops/05_AUDITS.md and the transport-selection research 2026-08-01.
hot_handover_enabled() { grep -qiE '^[[:space:]]*hot_handover:[[:space:]]*true' "$WIRELESS_CFG" 2>/dev/null; }
wireless_running() { pgrep -f /usr/sbin/carplay-wireless >/dev/null 2>&1; }
# Is ANY part of the wireless stack still drawing power? The advertiser can be dead while hostapd is
# still beaconing and hci0 is still UP — which is exactly the state that made a wired session crawl
# (owner report 2026-08-11), so all three are checked, not just the daemon.
wireless_stack_up() {
  wireless_running && return 0
  pgrep -f "[h]ostapd" >/dev/null 2>&1 && return 0
  hciconfig hci0 2>/dev/null | grep -q "UP RUNNING" && return 0
  return 1
}
# Is ANY airplayd alive, in EITHER spawn form — the WIRED supervisor's bare-name `setsid airplayd`
# (argv[0]=="airplayd") or the wireless av.rs full-path `/usr/sbin/airplayd`? Exact-match probes for each
# form (deliberately NOT `pgrep -f airplayd`, which would false-match a transient `tail`/`grep` of an
# *airplayd*.log and wrongly report it alive). Used by the stale-flag watchdog to tell a truly-dead
# wireless session from a live one. (Over-reporting alive would only DELAY the watchdog, never mis-clear.)
airplayd_alive() { pgrep -x airplayd >/dev/null 2>&1 || pgrep -x /usr/sbin/airplayd >/dev/null 2>&1; }
# Pairing association model from the host YAML `pairing:` (default just_works — the proven CCPA posture).
# `numeric_comparison` selects SSP DisplayYesNo → the iPhone + box both show a 6-digit code to match.
# Passed to carplay-wireless as CARPLAY_PAIRING_MODE (its ssp_agent reads the env).
pairing_mode() {
  if grep -qiE '^[[:space:]]*pairing:[[:space:]]*(numeric|numeric_comparison)' "$WIRELESS_CFG" 2>/dev/null; then
    echo numeric
  else
    echo just_works
  fi
}
# Should the BOX raise its own SoftAP? Default TRUE (stock behavior); only an explicit `wifi_ap: false`
# in the host YAML suppresses it. This is the gm_ccpa bridge role: the head-unit app is the AirPlay
# endpoint on the VEHICLE's hotspot, so the box must be the Bluetooth radio and the MFi coprocessor
# and nothing else. bt_on.sh is independent of Wi-Fi (IW416 BT is a UART/hci_uart path, not the moal
# SDIO driver), so dropping wlan_on.sh leaves Bluetooth fully working.
wifi_ap_enabled() { ! grep -qiE '^[[:space:]]*wifi_ap:[[:space:]]*false' "$WIRELESS_CFG" 2>/dev/null; }

# Pull one scalar out of the host YAML, stripping any surrounding quotes. Values may contain spaces
# (an SSID like `myChevrolet 32D4`), so everything after the colon is taken verbatim.
cfg_value() {
  # Case-SENSITIVE on purpose: a case-insensitive grep paired with a case-sensitive sed strip would
  # match `WIFI_SSID:` and then strip nothing, leaking the whole line in as the value. Trailing
  # whitespace/CR is removed before the quote strip so a deliberately-quoted trailing space survives.
  grep -E "^[[:space:]]*$1:" "$WIRELESS_CFG" 2>/dev/null | head -1 |
    sed -e "s/^[[:space:]]*$1:[[:space:]]*//" -e 's/[[:space:]]*$//' -e 's/^"\(.*\)"$/\1/' -e "s/^'\(.*\)'$/\1/"
}

# gm_ccpa bridge role: the 0x5702->0x5703 handoff must hand the iPhone the VEHICLE's hotspot, not the
# box's own AP. `wifi_handoff::read_hostapd_ap_config()` reads /etc/hostapd.conf unconditionally, so
# the cheapest correct intervention is to write the app-supplied credentials into that file before
# carplay-wireless starts. This is safe precisely BECAUSE wifi_ap:false means no hostapd ever runs
# from it. /etc/hostapd.conf.stock is the pristine base, so repeated runs never drift.
apply_host_wifi_creds() {
  # NOTE the underscore prefixes. The bare names s/p/c would CLOBBER the main loop's presence
  # variables ($p is the current /tmp/host_present value): wireless_up runs from inside the edge
  # handler BEFORE last_p is updated, so overwriting $p manufactures a fresh 0->1 edge on every
  # subsequent tick. That drives concurrent radio bring-ups and then the flap detector, escalating
  # to ocbmd restarts and real reboots. Observed live before this was caught.
  _s=$(cfg_value wifi_ssid)
  [ -n "$_s" ] || return 0
  _p=$(cfg_value wifi_pass)
  _c=$(cfg_value wifi_channel)
  if [ ! -f /etc/hostapd.conf.stock ]; then
    cp /etc/hostapd.conf /etc/hostapd.conf.stock || {
      echo "[sup] WARN: could not snapshot /etc/hostapd.conf — not rewriting credentials"; return 0; }
  fi
  _tmp=/etc/hostapd.conf.new   # same filesystem, so the mv below is atomic (jffs2)
  # Rebuild rather than sed-substitute: an SSID or passphrase can contain regex/delimiter characters.
  # Only strip the keys we are actually going to replace, so an unsupplied channel keeps the stock one
  # (hostapd of this vintage has no ACS and refuses to start with no channel).
  _strip='^ssid='
  [ -n "$_p" ] && _strip="$_strip|^wpa_passphrase="
  [ -n "$_c" ] && _strip="$_strip|^channel="
  grep -vE "$_strip" /etc/hostapd.conf.stock > "$_tmp" 2>/dev/null || {
    echo "[sup] WARN: could not read the stock hostapd.conf — not rewriting credentials"; return 0; }
  echo "ssid=$_s" >> "$_tmp"
  if [ -n "$_p" ]; then
    echo "wpa_passphrase=$_p" >> "$_tmp"
    # read_hostapd_ap_config() reports Wpa2OrWpa3Personal only when a wpa=/wpa_key_mgmt= line is
    # ALSO present; without one it drops the passphrase and advertises the vehicle's WPA hotspot as
    # OPEN in the 0x5703. The phone then fails to join with no error on our side.
    grep -qE '^(wpa|wpa_key_mgmt)=' "$_tmp" || { echo 'wpa=2' >> "$_tmp"; echo 'wpa_key_mgmt=WPA-PSK' >> "$_tmp"; }
  fi
  [ -n "$_c" ] && echo "channel=$_c" >> "$_tmp"
  mv "$_tmp" /etc/hostapd.conf && sync
  echo "[sup] 0x5703 credentials <- host app: ssid='$_s' channel='${_c:-stock}' pass=$([ -n "$_p" ] && echo set || echo none)"
}

wireless_up() {
  if radio_off; then
    echo "[sup] radios inhibited (CT_RADIO off) — skipping wireless bring-up"
    return
  fi
  if ! wireless_enabled; then
    echo "[sup] wireless disabled by config (wireless: false) — wired-only"
    wireless_running && wireless_down   # config flipped to false mid-present: honor it
    return
  fi
  # THE CHOKE POINT (owner directive 2026-08-11). Never raise the wireless stack while a WIRED session
  # owns the box and Hot-Handover is off. Guarding the CALLERS was not enough: FOUR paths reach here
  # during a live wired session — the host-presence 0->1 edge (:747), A2's own restore arm, the
  # deferred `wireless_rebring_at` timer (:721, armed by /tmp/wireless_restart or a CT_RADIO edge),
  # and the synthetic 0->1 edge a RESPAWNED supervisor manufactures from `last_p=""`. All four funnel
  # through this function, so the check belongs here.
  #
  # This is what actually delivers "wired => radios off". A2's teardown was already working — the
  # captured log shows it going quiet after two firings, which only happens when `wireless_stack_up`
  # reads false — and then a bring-up put hostapd, wlan0 and hci0 straight back.
  if ! hot_handover_enabled && phone_on_bus && ! wireless_owns_session; then
    echo "[sup] wireless_up SUPPRESSED — a WIRED session owns the box (Hot-Handover off)"
    return
  fi
  # Same choke point for Android Auto. TWO conditions, because there is a race: wireless_up() fires on
  # the host-present 0->1 edge (at SUBSCRIBE), but AA only claims the owner flag a few seconds later
  # (after the app opens CH_IP and aa-bridge finishes the AOAP switch). So suppressing on
  # aa_owns_session ALONE let wireless CP start BT pairing in that window even with an Android phone
  # plugged. Also suppress when an Android phone is simply ON THE BUS and AA is enabled — that phone is
  # the wired AA path, exactly as a wired iPhone (phone_on_bus above) suppresses wireless. (docs/host/02_ANDROID_AUTO.mda.)
  if aa_owns_session || { android_phone_on_bus && aa_enabled; }; then
    echo "[sup] wireless_up SUPPRESSED — a wired Android Auto phone owns/claims the box"
    return
  fi
  wireless_running && return   # idempotent guard (checked HERE, before the wrapper exists)
  # Clean slate before a fresh bring-up: reap any orphaned A/V children left by a prior session so the
  # new stack can NEVER latch onto / collide with a stale airplayd or rx-connect (the observed
  # "connection unsuccessful" after an app close+reopen). Safe here because this runs only on the
  # host-absent->present edge (app just (re)connected) with wireless enabled, and wlan_on/bt_on below
  # give the SIGTERM'd stragglers a full settle before carplay-wireless spawns its own children.
  #
  # docs/wireless/00_WIRELESS_CARPLAY.md #1.4 (extended, review finding 2026-07-24): this pkill previously ran UNCONDITIONALLY, which
  # meant the CCPA-tab "Restart wireless"/Forget trigger (`wireless_down` then, ~4s later, THIS
  # function — see the main loop's `wireless_rebring_at` handling) could kill a live WIRED session's
  # airplayd/rx-connect exactly like the bug #1.4 already fixed inside `wireless_down` itself. Only reap
  # here if they are NOT a live wired session: either the transport flag already says "wireless" (a
  # stale, uncleaned prior wireless session — safe and correct to reap), or neither process is even
  # running (nothing to protect).
  _wl_reaped=0
  if wireless_owns_session || { ! pgrep -x airplayd >/dev/null 2>&1 && ! pgrep -x rx-connect >/dev/null 2>&1; }; then
    _wl_reaped=1
    pkill -f airplayd 2>/dev/null
    pkill -f rx-connect 2>/dev/null
  else
    echo "[sup] wireless_up: airplayd/rx-connect are wired-owned — leaving them running, skipping the pre-bringup reap"
  fi
  mode=$(pairing_mode)
  if wifi_ap_enabled; then ap=1; else ap=0; fi
  apply_host_wifi_creds
  # In the bridge role the HEAD-UNIT APP is the AirPlay endpoint, so the box must not spawn one.
  # av.rs resolves both daemons through these env vars (av.rs:145-146, 238-239), so pointing them at
  # a no-op suppresses the A/V layer with no Rust change. Note this also means av.rs's rollback path
  # runs and /tmp/carplay_transport is released — which is the behaviour we want for a no-A/V box.
  # DO NOT point these at /bin/true. av.rs's wait_visible polls for the spawned path to become
  # visible to pgrep, and pid_alive requires argv[0] to EQUAL the full path — so a binary that exits
  # immediately (or any shell script, whose argv[0] is the interpreter) can never satisfy it. The
  # result was ~12s stalled under AV_LOCK on EVERY iOS 0x5702 retry, blocking the bt-driver loop;
  # measured on hardware, four retries wedged the control session long enough for the phone to drop
  # Bluetooth — which is the session anchor, so the whole CarPlay attempt died with it.
  #
  # Letting the box's own airplayd/rx-connect start is harmless in the bridge role: with wifi_ap:false
  # there is no wlan0 and the box has no route to the vehicle subnet, so nothing can reach them. They
  # latch AV_LAYER_UP instantly, the stall disappears, and the head-unit app remains the only
  # reachable AirPlay endpoint. Suppressing them properly needs an explicit AV_DISABLED gate in
  # av.rs, which is a Rust change.
  AV_SUPPRESS=''
  echo "[sup] host PRESENT + wireless enabled -> CLEAN bring-up of wireless stack (detached, pairing=$mode, wifi_ap=$ap, av_suppress=no)"
  # Detached session; sequential WiFi AP -> BT radio -> CarLink advertiser, each redirected (not piped).
  # NO inner pgrep guard: the wrapper `sh -c` argv itself contains "/usr/sbin/carplay-wireless", so an
  # inner `pgrep -f /usr/sbin/carplay-wireless` self-matches the wrapper and SKIPS the launch (the bug
  # that left carplay-wireless never started). The outer `wireless_running && return` above is the guard.
  # CARPLAY_PAIRING_MODE is inherited through setsid -> sh -c -> the exec'd carplay-wireless.
  # CARPLAY_WIFI_AP is read by the inner shell at runtime (the body is single-quoted on purpose, so the
  # decision travels as an env var rather than by interpolating into the wrapper's argv).
  # shellcheck disable=SC2086  # AV_SUPPRESS is a deliberate word-split of VAR=VAL assignments
  env $AV_SUPPRESS CARPLAY_PAIRING_MODE="$mode" CARPLAY_WIFI_AP="$ap" OCBM_WL_REAPED="$_wl_reaped" setsid sh -c '
    # Radio bring-up goes through the chipset-neutral seam. It resolves this unit'"'"'s own
    # bring-up mapping at runtime, so the supervisor never names a chip, a module or an attach
    # helper. Exit codes: 0 converged / 1 failed / 2 already up / 3 unsupported on this variant.
    # Previously these were /script/wlan_on.sh and /script/bt_on.sh — the IW416 baseline'"'"'s own
    # scripts, which simply do not exist on a Realtek or Broadcom unit, so wireless bring-up
    # failed SILENTLY here (the calls sit in this detached wrapper, redirected, exit status
    # unread) while wired projection kept working and nothing looked wrong.
    if [ "$CARPLAY_WIFI_AP" = "0" ]; then
      echo "[sup] wifi_ap:false -> box SoftAP SUPPRESSED (BT-only bridge role; the head-unit app owns Wi-Fi)" >/tmp/wlan.log
    else
      sh /script/radio_hal.sh wifi_ap_on >/tmp/wlan.log 2>&1
    fi
    sh /script/radio_hal.sh bt_on >/tmp/bt.log 2>&1
    # docs/wireless/00_WIRELESS_CARPLAY.md #1.3 (review finding 2026-07-24): truncate the wireless health log HERE, immediately
    # before exec, not right after the (SIGTERM-only, kill-unconfirmed) reap above — truncating early
    # left a window where the OLD airplayd could still flush its buffered stdio (exit-time flush of an
    # O_APPEND-opened, fully-buffered log) and replay the PRIOR session'"'"'s "pair-verify OK"/"RECORD
    # done" lines back in after the truncate, exactly the bogus-instant-health bug this truncation
    # exists to prevent.
    #
    # That fix used to rely on wlan_on.sh + bt_on.sh taking ~15s, which gave the SIGTERM'"'"'d
    # stragglers a real settle window "for free". Behind the seam that assumption is gone in BOTH
    # directions: a variant with nothing to do, an already-converged fast path, or an honest
    # "unsupported" exit all return in milliseconds. So confirm the reap actually completed
    # instead of inferring it from elapsed time — this is strictly stronger than the old timing,
    # under which a SIGTERM-ignoring straggler survived even the 15s.
    #
    # Gated on OCBM_WL_REAPED: in the wired-owned skip branch a live wired airplayd is EXPECTED
    # and writes to its own log, not this one, so it must not burn the bound.
    if [ "$OCBM_WL_REAPED" = 1 ]; then
      _i=0
      while [ "$_i" -lt 25 ] && { pgrep -x airplayd || pgrep -x /usr/sbin/airplayd \
            || pgrep -x rx-connect || pgrep -x /usr/sbin/rx-connect; } >/dev/null 2>&1; do
        _i=$((_i + 1)); sleep 0.2
      done
    fi
    : > /tmp/airplayd_wl.log
    exec /usr/sbin/carplay-wireless </dev/null >/tmp/wl.log 2>&1
  ' </dev/null >/dev/null 2>&1 &
}

wireless_down() {
  # $1 = why, for the log only. Default keeps the historical host-presence wording; the wired-takeover
  # caller passes its own so the log never claims the app went away when it did not.
  _wd_why="${1:-host GONE (or wireless disabled)}"
  # COMPLETE wireless-stack teardown, tied to app presence: the advertiser (carplay-wireless) AND its
  # setsid-detached A/V children (airplayd, rx-connect). pkill-ing ONLY the parent orphans the children,
  # and the next app-connect bring-up then collides with those orphans ("Address in use" on the
  # advertiser / a stale AirPlay receiver) -> iOS reports "connection unsuccessful". No head-unit app
  # means no possible session (wired OR wireless), so reaping the whole A/V stack here is correct and
  # returns the box to the clean waiting state. Reap even if carplay-wireless already died (the
  # detached children can outlive it).
  # `airplayd_alive` not `pgrep -x airplayd`: BusyBox `-x` matches the FULL invoked path, so a bare
  # `-x airplayd` is BLIND to the wireless spawn form `/usr/sbin/airplayd` (device-proven, docs/wireless/00_WIRELESS_CARPLAY.md).
  # A stranded wireless airplayd would otherwise trip this early return and never be reaped.
  if ! wireless_running && ! airplayd_alive && ! pgrep -x rx-connect >/dev/null 2>&1 \
     && ! pgrep -x hostapd >/dev/null 2>&1 && ! { hciconfig hci0 2>/dev/null | grep -q "UP"; }; then
    return   # already fully down — processes dead AND radios actually powered off (docs/carplay/04_CAPABILITIES_AND_CONFIG.md: a
             # crashed stack can leave hostapd beaconing / hci0 UP with all three daemons gone;
             # the early return must not skip the radio teardown in that stranded state)
  fi
  if wireless_owns_session; then
    echo "[sup] $_wd_why -> tearing down COMPLETE wireless stack (advertiser + A/V children)"
    # Stop the CarLink advertiser + the A/V daemons + drop the AP (the WiFi beacon is the main drain).
    # BT end-state (docs/carplay/04_CAPABILITIES_AND_CONFIG.md radio gating): module stays ATTACHED (hciattach kept — re-attach is the
    # flaky part) but the radio is POWERED DOWN (noscan, then hci0 down at the end of this block) so
    # an app-less box drops ACLs and answers nothing. SIGTERM first, brief settle, then SIGKILL only the A/V daemons (NOT the BT hci path
    # — heavy -9 on BT bring-up is what wedges the IW416 controller).
    # SELF-MATCH FIX 2026-08-01: the patterns MUST use the `[x]`-char-class form. This block runs as
    # `setsid sh -c '<this text>'`, so its own argv literally contains "/usr/sbin/carplay-wireless",
    # "airplayd", "rx-connect" — a plain `pkill -f /usr/sbin/carplay-wireless` SIGKILLs THIS subshell
    # before it reaches wlan_off, orphaning the AP (hostapd kept broadcasting → iOS shows the phone still
    # on WiFi with CarPlay inactive). `[/]usr/...` matches the real daemon but NOT this text (verified:
    # busybox pgrep `[h]ostapd` matches, `[z]hostapd` doesn't). Same footgun the wireless_up guard notes.
    setsid sh -c '
      pkill -f "[/]usr/sbin/carplay-wireless" 2>/dev/null
      pkill -f "[a]irplayd" 2>/dev/null
      pkill -f "[r]x-connect" 2>/dev/null
      sleep 1
      pkill -9 -f "[a]irplayd" 2>/dev/null
      pkill -9 -f "[r]x-connect" 2>/dev/null
      pkill -9 -f "[/]usr/sbin/carplay-wireless" 2>/dev/null
      hciconfig hci0 noscan 2>/dev/null
      sh /script/radio_hal.sh wifi_ap_off >/tmp/wlan_off.log 2>&1
      # docs/carplay/04_CAPABILITIES_AND_CONFIG.md radio gating: POWER the BT radio off (page/inquiry dead, ACLs dropped), not just
      # noscan — an app-less box must not keep the iPhone BT-connected. hci0 down (NOT bt_off.sh,
      # whose rmmod path is the wedge-prone one); the next bring_up() does its proven down->up cycle.
      hciconfig hci0 down 2>/dev/null
    ' </dev/null >/dev/null 2>&1 &
    # docs/wireless/00_WIRELESS_CARPLAY.md #1.2/#1.3: this is OUR session ending — clear the health state we own and the transport
    # flag ourselves (value-scoped, so it never fires against a wired session's "wired"/absent value),
    # rather than relying on the (possibly already-dying) child processes to clean up after themselves.
    saw_paired=0; saw_record=0; healthy=0; established_since=0; write_healthy 0
    [ "$(cat /tmp/carplay_transport 2>/dev/null)" = wireless ] && rm -f /tmp/carplay_transport
    # audit 3.3: the phase mirror is last-write-wins with no unlink, so a leftover value reads as
    # "phone detected" to every fresh subscriber. carplay-wireless idles it on its own exit paths;
    # this covers the case where we just SIGKILLed it before it got there.
    rm -f /tmp/bt_phase
  else
    # docs/wireless/00_WIRELESS_CARPLAY.md #1.4: airplayd/rx-connect are WIRED-owned right now (or nothing is running at all) — never
    # kill them from here. Previously this function's pkill list had no such guard, so a CCPA-tab
    # "Restart wireless"/Forget action during a live WIRED session killed the wired airplayd too — a
    # real regression, not a wireless-only effect. Only the wireless-specific advertiser + radios are
    # ours to tear down in this case.
    echo "[sup] $_wd_why -> tearing down wireless advertiser + radios only (airplayd/rx-connect are wired-owned, left running)"
    # SELF-MATCH FIX 2026-08-01: `[/]usr/...` so this `sh -c` block doesn't SIGKILL itself before wlan_off
    # (see the COMPLETE-teardown branch above for the full rationale).
    setsid sh -c '
      pkill -f "[/]usr/sbin/carplay-wireless" 2>/dev/null
      sleep 1
      pkill -9 -f "[/]usr/sbin/carplay-wireless" 2>/dev/null
      hciconfig hci0 noscan 2>/dev/null
      sh /script/radio_hal.sh wifi_ap_off >/tmp/wlan_off.log 2>&1
      # docs/carplay/04_CAPABILITIES_AND_CONFIG.md radio gating: power BT off here too (see the COMPLETE-teardown branch rationale).
      hciconfig hci0 down 2>/dev/null
    ' </dev/null >/dev/null 2>&1 &
  fi
}

write_healthy 0
echo "[sup] up; IDLE — gating the CarPlay session on $FLAG (waiting for a host app)"
WIRELESS_RESTART_FLAG=/tmp/wireless_restart   # ocbmd touches this on the CCPA tab's Restart-wireless / Forget
wireless_rebring_at=""                         # epoch(uptime) to re-bring-up after a requested restart
wireless_was_owner=0    # tracks wireless_owns_session across ticks (see the reset check below)
last_r=0                # tracks the CT_RADIO inhibit flag across ticks (docs/carplay/04_CAPABILITIES_AND_CONFIG.md radio gating)

# Startup reconciliation (docs/carplay/04_CAPABILITIES_AND_CONFIG.md radio gating, gap G3): a respawned supervisor starts with
# last_p="" and never sees a 1->0 edge, so a crash between GONE and teardown would strand radios
# ON with no app. If the app is absent but any radio state is up, reconcile once now. No-op on a
# clean boot (boot chain raises no radios).
if [ "$(cat "$FLAG" 2>/dev/null)" != 1 ]; then
  if wireless_running || pgrep -x hostapd >/dev/null 2>&1 || hciconfig hci0 2>/dev/null | grep -q "UP"; then
    echo "[sup] startup reconciliation: app absent but radio state up -> wireless_down (stranded by a prior crash)"
    wireless_down
  fi
fi

while :; do
  # Read presence, acting only on a DEFINITE 0/1 — an empty/partial read (or missing file) keeps the
  # last state rather than flapping. (ocbmd writes the flag atomically, so this is defensive.)
  p=$(cat "$FLAG" 2>/dev/null)

  # docs/wireless/00_WIRELESS_CARPLAY.md #1.3 (extended, review finding 2026-07-24): `wireless_down`'s own branch already resets
  # saw_paired/saw_record/healthy when IT tears a wireless session down — but a wireless session can
  # ALSO end via `teardown_av_layer()` (crates/vendor/wireless/src/av.rs, called on preempt/shutdown),
  # which removes /tmp/carplay_transport directly without ever calling wireless_down. Without this
  # edge-detect, that second exit path left stale healthy=1/saw_record=1 latched — the supervisor would
  # keep publishing phase=STREAMING (and /tmp/session_healthy=1) for a session that already ended,
  # until happenstance triggered a wired arm() to reset them (never, if no USB phone is attached).
  # Checked every tick, transport-agnostic of WHICH exit path fired.
  if wireless_owns_session; then
    wireless_was_owner=1
  elif [ "$wireless_was_owner" = 1 ]; then
    wireless_was_owner=0
    echo "[sup] wireless session ended (transport flag cleared) -- resetting health state"
    saw_paired=0; saw_record=0; healthy=0; established_since=0; write_healthy 0
  fi

  # (C) Stale-flag watchdog (dual-transport self-heal): /tmp/carplay_transport is only ever cleared by
  # carplay-wireless's teardown_av_layer() (av.rs) on a CLEAN SIGTERM. A wireless session that dies
  # WITHOUT that (crash / SIGKILL / OOM — panic=abort does no unwinding) leaves the flag stuck at
  # "wireless" forever, which suppresses the ENTIRE wired path (arm/kill_session/escalate all gate on
  # wireless_owns_session). If the flag says wireless but NEITHER carplay-wireless NOR any airplayd is
  # alive for STALE_FLAG_N consecutive ticks, the owning session is provably gone — clear the orphaned
  # flag. Consecutive (not instant) so we never race a carplay-wireless momentarily between claims or an
  # airplayd mid-(re)spawn. Requiring airplayd ALSO dead keeps this maximally conservative: it never
  # clears a flag while any airplayd could still be serving a screen (the preempt edge above handles a
  # live-but-superseded wireless session when a real wired cable appears).
  if wireless_owns_session && ! wireless_running && ! airplayd_alive; then
    stale_flag_ticks=$((stale_flag_ticks + 1))
    if [ "$stale_flag_ticks" -ge "$STALE_FLAG_N" ]; then
      echo "[sup] STALE transport flag: 'wireless' with no carplay-wireless/airplayd for $stale_flag_ticks ticks -> clearing (self-heal, no reboot)"
      rm -f /tmp/carplay_transport
      stale_flag_ticks=0
    fi
  else
    stale_flag_ticks=0
  fi

  # CCPA-tab wireless restart (ocbmd sets the flag for Restart-wireless, Forget-all, Forget-device): tear
  # carplay-wireless down now, then re-bring-up a few ticks later (once it's actually gone) if a host is
  # still present. This is how the app's box controls + a forget (reload with cleared keys) take effect.
  if [ -f "$WIRELESS_RESTART_FLAG" ]; then
    rm -f "$WIRELESS_RESTART_FLAG"
    echo "[sup] wireless restart requested (CCPA tab)"
    wireless_down
    wireless_rebring_at=$(( $(now) + 4 ))
  fi
  if [ -n "$wireless_rebring_at" ] && [ "$(now)" -ge "$wireless_rebring_at" ]; then
    wireless_rebring_at=""
    [ "${last_p:-0}" = 1 ] && wireless_up
  fi

  # App-commanded radio inhibit edges (CT_RADIO -> /tmp/radio_off, docs/carplay/04_CAPABILITIES_AND_CONFIG.md radio gating). Flag
  # appeared -> radios down NOW (wireless_down's docs/wireless/00_WIRELESS_CARPLAY.md #1.4 guard protects a wired-owned
  # airplayd). Flag cleared -> bring-up rides the existing 4 s wireless_rebring_at deferral, NOT a
  # direct wireless_up: (a) a quick off->on toggle must not race the off-edge's detached teardown
  # (same class the Restart-wireless deferral exists for), and (b) go_idle clears this flag and
  # host_present in the same instant, so an on-edge can arrive on the very tick the app went away
  # with last_p still stale — the deferred handler re-checks presence at fire time.
  if radio_off; then r=1; else r=0; fi
  if [ "$r" = 1 ] && [ "$last_r" != 1 ]; then
    echo "[sup] CT_RADIO off -> tearing radios down (app-commanded)"
    wireless_down
  fi
  if [ "$r" = 0 ] && [ "$last_r" = 1 ]; then
    echo "[sup] CT_RADIO on -> scheduling radio bring-up (deferred past teardown settle)"
    wireless_rebring_at=$(( $(now) + 4 ))
  fi
  last_r="$r"

  # presence 0->1 edge accounting (input to the flap detector) + app-driven wireless bring-up/teardown
  case "$p" in
    0|1)
      if [ "$p" = 1 ] && [ "$last_p" != 1 ]; then
        edge_ts="$edge_ts $(now)"
        wireless_up      # host appeared: bring wireless up alongside the always-on wired path (gated)
      fi
      if [ "$p" = 0 ] && [ "$last_p" = 1 ]; then
        wireless_down    # host went away: idle the wireless radios (wired baseline stays ready)
      fi
      last_p="$p"
      ;;
  esac

  case "$p" in
    1)
      # Phone-presence gate (2026-07-12): with a host present, check the bus EVERY tick. No iPhone
      # = a legitimate WAIT state — do NOT attempt projection, do NOT count failures, and above all
      # do NOT escalate the ladder (phone_resets/L2 fired at an intentionally-absent phone were what
      # wedged the staged-timing tests). The moment the phone appears, arm IMMEDIATELY (clear any
      # backoff) — this is the plug-triggered retry that cuts plug→pixels to protocol time.
      if phone_on_bus; then
        phone_absent_ticks=0
        write_phone_flag 1
        if [ "$phone_waiting" = 1 ]; then
          phone_waiting=0
          proj_fails=0
          backoff_until=0
          echo "[sup] iPhone appeared on the bus — arming NOW"
        fi
      else
        # Debounce (3 ticks ≈ 3 s): the role-switch transition briefly reads as absent in BOTH modes
        # (device detached, gadget not yet CONFIGURED) — never flicker a false "waiting for phone".
        phone_absent_ticks=$(( ${phone_absent_ticks:-0} + 1 ))
        if [ "$phone_absent_ticks" -ge 3 ]; then
          write_phone_flag 0
          if [ "$armed" = 1 ]; then
            : # mid-session unplug: let the health/death paths handle teardown naturally
          else
            [ "$phone_waiting" != 1 ] && echo "[sup] host PRESENT, no iPhone attached — waiting for plug (no escalation)"
            phone_waiting=1
          fi
        fi
      fi
      # (A) Dual-transport preempt edge — GATED on the opt-in Hot-Handover lever (default OFF = Standard,
      # spec-conformant: a cable plugged into a live wireless session is left charge-only, wireless keeps
      # running). Only when the host enables `hot_handover: true` does a genuine wired iPhone on the USB bus
      # preempt a wireless session. The guard's wired_iphone_on_usb() is the RAW 05ac bus probe, NOT
      # wireless_owns_session (the flag that can lie), so this fires ONLY for a real cable and never during a
      # healthy wireless-only session. preempt_wireless_for_wired() returns only once the flag is clear, so
      # the wired arm path below then runs (wireless_owns_session is now false).
      if hot_handover_enabled && wireless_owns_session && wired_iphone_on_usb; then
        preempt_wireless_for_wired
      fi
      # (A2) STANDARD-MODE RADIO GATING — the mirror of (A), owner directive 2026-08-11.
      #
      # (A) stops a cable PREEMPTING a live wireless session when Hot-Handover is off. Nothing did the
      # reverse: `wireless_down` is gated on HOST PRESENCE ("no head-unit app means no possible
      # session"), so once a wireless session ended and a wired one armed, hostapd kept beaconing,
      # wlan0 stayed up and hci0 stayed powered for the whole wired session — observed on a 528 MHz
      # single core as "serious performance issues".
      #
      # EDGE-TRIGGERED, DELIBERATELY. Every other `wireless_down` caller in this file fires on an edge
      # (host 1->0, CT_RADIO, the restart flag, startup reconciliation). An earlier revision of this
      # block evaluated a LEVEL every tick, which is unsafe here: `wireless_down` is a detached
      # `setsid` doing `sleep 1` + `wlan_off.sh` (which `rmmod moal` / `rmmod mlan`), so it stays
      # in-flight for seconds while `wireless_stack_up` is still true. A level trigger therefore
      # spawns a new teardown every second and stacks concurrent rmmods on the same module — the
      # driver-wedge class `wireless_up`'s own `wireless_running && return` guard exists to avoid.
      #
      # PREDICATE: `phone_on_bus`, NOT `wired_iphone_on_usb`. The raw probe greps Apple VID 05ac and
      # its own comment says what that means: "once a phone role-switches into projection it stops
      # enumerating as 05ac ... so this matches a FRESHLY-plugged cable — exactly the preempt trigger
      # — not an already-projected wired session." That is the opposite of the window A2 needs, and
      # using it made A2 a self-cancelling oscillator: tear down on plug, then `projection_up.sh`
      # switches the gadget to 08e4, 05ac vanishes, the falling edge restores the radios for the rest
      # of the wired session. `phone_on_bus` was hardened in July for this same class of bug and
      # covers both modes; its `wireless_owns_session` clause is unreachable under the negation below.
      _a2_want=0
      if ! hot_handover_enabled && phone_on_bus && ! wireless_owns_session; then
        _a2_want=1
      fi
      if [ "$_a2_want" != "${_a2_state:-0}" ]; then
        if [ "$_a2_want" = 1 ]; then
          # RISING edge: a wired session now owns the box. Claim the restore obligation ONLY if a
          # teardown actually ran — setting it unconditionally made A2 "restore" a stack it had never
          # touched, fighting the host-presence edge that owns bring-up.
          if wireless_stack_up; then
            wireless_down "WIRED session active + Hot-Handover OFF (radios yield to the cable)"
            radios_yielded_to_wired=1
          fi
        elif [ "${radios_yielded_to_wired:-0}" = 1 ]; then
          # FALLING edge: cable gone, or Hot-Handover was switched on. Test the guards BEFORE clearing
          # the flag — clearing first meant a restore could be dropped and the radios left down for
          # the rest of the app session with no way back short of an app reconnect.
          if wireless_enabled; then
            radios_yielded_to_wired=0
            echo "[sup] wired session ended -> restoring the wireless stack"
            wireless_up
          fi
        fi
        _a2_state="$_a2_want"
      fi
      if aa_owns_session; then
        # Android Auto owns ci_hdrc.0 right now (aa-bridge holds a live AOAP link + the wired-aa owner
        # flag). Hands off entirely: no projection_up, no arm(), no escalation — aa-bridge self-manages
        # its session and the step-1 guards already suppress kill_session/escalate. (docs/host/02_ANDROID_AUTO.mdc.)
        :
      elif android_phone_on_bus && ! wired_iphone_on_usb && ! carplay_session_live \
           && aa_enabled && ! wireless_owns_session; then
        # An Android phone (not an iPhone) is on the bus and AA is enabled -> SELECT Android Auto:
        # launch/relaunch the bridge. Runs before the CarPlay arm path so an Android phone never burns
        # a projection_up(05ac) attempt (audit scenario 2), and defers to a live wireless session.
        arm_aa
      elif wireless_owns_session; then
        # docs/wireless/00_WIRELESS_CARPLAY.md #1.3: a live wireless session owns airplayd right now. `armed` stays 0 for the whole
        # duration (arm()/kill_session()/escalate() all suppress themselves against wireless), so
        # WITHOUT this branch the wired logic below would keep calling arm() — which returns 1 here —
        # churn proj_fails, and eventually fire escalate("projection-failure") against a session the
        # wired supervisor doesn't manage; the phase would also misreport ARMING/IDLE instead of
        # STREAMING for a perfectly healthy wireless session. Track ITS health independently instead:
        # no arm attempts, no escalation, just milestone scanning (transport-scoped inside
        # scan_milestones) so `healthy=1` — and therefore the STREAMING phase below — can actually be
        # reached for a wireless session.
        scan_milestones
      elif [ "$armed" = "0" ] && [ "$phone_waiting" = 1 ]; then
        : # waiting for the phone — publish phase below, skip arm/escalation entirely
      elif [ "$armed" = "0" ]; then
        if [ "$(now)" -ge "$backoff_until" ]; then
          arm; _armrc=$?
          if [ "$_armrc" = 0 ]; then
            proj_fails=0
          elif [ "$_armrc" = 2 ]; then
            # BOX MISCONFIGURED. Deterministic: it will fail identically on every retry, so counting
            # it toward PROJ_AT would march a healthy phone through phone_reset -> ocbmd restart ->
            # REBOOT and change nothing (measured 2026-08-27, two L1 resets for a missing binary).
            # Retry slowly so a fix pushed to the box is still picked up without a reboot, and say so
            # once a minute rather than every second.
            proj_fails=0
            if [ "$(now)" -ge "${env_fault_log_until:-0}" ]; then
              echo "[sup] NOT escalating: the box is misconfigured, not the phone — see the [proj] line above"
              env_fault_log_until=$(( $(now) + 60 ))
            fi
            backoff_until=$(( $(now) + 15 ))
          else
            # projection bring-up failed (e.g. iPhone not enumerating as 05ac after a disturbance).
            # Count it; after PROJ_AT in a row, escalate into the ladder so phone_reset re-enumerates
            # the iPhone (docs/carplay/02_SESSION_LIFECYCLE.md #1 — the gap that forced a manual reboot during the P0 deploy).
            proj_fails=$((proj_fails + 1))
            echo "[sup] projection bring-up failed (#$proj_fails)"
            if [ "$proj_fails" -ge "$PROJ_AT" ]; then
              escalate "projection-failure"
              proj_fails=0
            else
              backoff_until=$(( $(now) + 4 ))
            fi
          fi
        fi
      elif ! pgrep -x airplayd >/dev/null 2>&1 || ! pgrep -x rx-connect >/dev/null 2>&1 || ! pgrep -x iap2d >/dev/null 2>&1; then
        # any of the three session daemons died while PRESENT -> re-ARM (was: airplayd only)
        # (docs/wireless/00_WIRELESS_CARPLAY.md #1.1 hygiene: -x matches the process name, not -f's full-argv substring — avoids the
        # same false-match class already fixed in av.rs's Rust `running()`, e.g. a log tail/grep/editor
        # with "airplayd" in its own argv suppressing a needed re-ARM.)
        dead=""; for d in airplayd rx-connect iap2d; do pgrep -x "$d" >/dev/null 2>&1 || dead="$dead $d"; done
        fails=$((fails + 1))
        back=$((fails * 5)); [ "$back" -gt 30 ] && back=30
        backoff_until=$(( $(now) + back ))
        echo "[sup]$dead exited while PRESENT -> re-ARM after ${back}s (fail #$fails)"
        armed=0
        write_healthy 0
      else
        # armed + airplayd alive: derive establishment health, detect a stall
        scan_milestones
        if [ "$saw_record" = 0 ] && [ "$(now)" -ge "$estab_deadline" ]; then
          stuck_fails=$((stuck_fails + 1))
          echo "[sup] ESTABLISHMENT STALL (#$stuck_fails): ARMED but no RECORD within grace (paired=$saw_paired)"
          if [ "$stuck_fails" -ge "$L1_AT" ]; then
            escalate "establishment-stall"
          else
            kill_session; armed=0; write_healthy 0   # soft re-ARM (below L1 threshold)
            backoff_until=$(( $(now) + 2 ))
          fi
        fi
      fi
      ;;
    0)
      [ "$armed" = "1" ] && teardown
      apply_pending   # idle boundary — apply any deferred peer-store mutation (#25)
      ;;
    *) : ;;  # empty/unexpected read -> keep last state
  esac

  # --- presence-flap detector (the docs/carplay/02_SESSION_LIFECYCLE.md signature) -> escalation ladder ---
  prune_edges
  if [ "$edge_count" -ge "$FLAP_N" ] && [ "$(now)" -ge "$backoff_until" ]; then
    echo "[sup] flapping: $edge_count present-edges in ${FLAP_WINDOW}s"
    escalate "flapping"
  fi

  # --- counter reset: ONLY on a confirmed-established session held >= CONFIRM_HOLD (never in teardown) ---
  if [ "$healthy" = 1 ] && [ "$established_since" != 0 ] \
     && [ $(( $(now) - established_since )) -ge "$CONFIRM_HOLD" ]; then
    if [ "$stuck" != 0 ] || [ "$stuck_fails" != 0 ] || [ "$fails" != 0 ] \
       || [ "$l1_tries" != 0 ] || [ "$l2_tries" != 0 ]; then
      echo "[sup] session healthy ${CONFIRM_HOLD}s+ — clearing STUCK counters"
    fi
    stuck=0; stuck_reason=""; stuck_fails=0; fails=0; proj_fails=0; edge_ts=""; l1_tries=0; l2_tries=0
    # a confirmed-good session clears the persistent L3 reboot budget (the reboot cycle recovered)
    if [ -s "$REBOOT_BUDGET" ] && [ "$(cat "$REBOOT_BUDGET" 2>/dev/null)" != 0 ]; then
      rm -f "$REBOOT_BUDGET"; sync; echo "[sup] confirmed-good session — cleared L3 reboot budget"
    fi
    established_since=0   # one-shot clear so we don't re-log every tick while healthy
  fi

  # --- publish the canonical lifecycle verdict (#26) ---
  if   [ "$stuck" = 1 ];                                    then ph=STUCK;        rs="$stuck_reason"
  elif [ "$healthy" = 1 ];                                  then ph=STREAMING;    rs=""
  elif [ "$l1_tries" != 0 ] || [ "$l2_tries" != 0 ];       then ph=RECOVERING;   rs="self-heal l1=$l1_tries l2=$l2_tries"
  elif [ "$armed" = 1 ];                                    then ph=ESTABLISHING; rs="paired=$saw_paired"
  elif [ "$phone_waiting" = 1 ] && [ "${last_p:-0}" = 1 ]; then ph=WAITING_PHONE; rs="plug in the iPhone"
  elif [ "${last_p:-0}" = 1 ];                             then ph=ARMING;       rs=""
  else                                                          ph=IDLE;         rs=""
  fi
  # transition ring (#30): one line per PHASE change (never per tick), uptime-stamped for correlation;
  # count-bounded in bound_logs (NOT the lossy byte-tail, so a flap's ONSET survives).
  if [ "$ph" != "$last_phase" ]; then
    printf 't=%s %s -> %s reason=%s\n' "$(now)" "${last_phase:-BOOT}" "$ph" "$rs" >> "$RING" 2>/dev/null
    last_phase="$ph"
  fi
  write_state "$ph" "$rs"

  tick=$((tick + 1))
  [ $((tick % 30)) -eq 0 ] && bound_logs
  sleep 1
done
