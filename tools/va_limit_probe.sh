#!/bin/bash
# View-area limit prober: arm a spec, capture the PHONE's airplayd log across the attempt,
# classify, and extract the acceptance-check reason iOS gives when it refuses.
#
# The phone-side capture is the whole point: box logs say WHAT we declared, only the phone says WHY
# it refused. `-pn airplayd` is what makes this tractable — an unfiltered capture of the same window
# ran 4.4 GB and still missed the line.
# Requires WIRELESS CarPlay: the iPhone's USB port must be free to talk to this Mac.
#
# ARMED THROUGH THE APP DOOR (migrated 2026-10-19). The rect is written with the app's own
# `viewarea arm <WxH@X,Y>` + `save` (ControlServer, the same model fields the Settings form writes)
# and reaches the box in the pushed config's `viewAreas[1]`. It used to be pushed to the box file
# /tmp/carplay_viewarea2 with ocbm-host; since 2026-10-19 that bench lever is NO-CONFIG-ONLY
# (owner decision D3, docs/carplay/04_CAPABILITIES_AND_CONFIG.md §2): with this app connected, its
# pushed config is in force and carplayd logs the lever as IGNORED. Two consequences:
#   * `:initial` has NO app equivalent (the app model has no initial flag), so a spec ending in
#     `:initial` is refused here. The session therefore starts in area [0]; once it is up this
#     probe asks for area [1] with `viewarea request 1`, which is what puts the rect on screen.
#   * the app's own ViewArea2Rule (containment, parity, positivity, the 800x480 / 480x800 product
#     floor) now gates what can be probed: a rect it refuses scores REFUSED LOCALLY. Sub-floor sizes
#     are measurable only APP-LESS (lever file + no host config in force), which this probe — it
#     drives the app — cannot do.
#
# VERDICTS (2026-10-19 review): ACCEPTED means the rect was SHOWN — the `viewarea request 1` reply
# was {"ok":true}, `get viewarea` then reported the armed rect (within RECT_TOL), the box did not
# log `host viewArea index=1 REFUSED`, and no teardown followed for 10 s. A request the app would
# not send, a rect that never landed, or a box-side REFUSED index all score INCONCLUSIVE.
SPEC="$1"; LABEL="$2"; CM="${3:-false}"; FT="${4:-false}"
CTRL_PORT=${CTRL_PORT:-8765}
TRANSITION_TIMEOUT=${TRANSITION_TIMEOUT:-30}   # s for the observed rect to reach the armed one
CAP=""; DONE=0; LAUNCHED=0
# Cleanup on EVERY exit. A run that ends before a verdict (NOT RUN, REFUSED LOCALLY, app failed to
# answer) must not leave this probe's app instance or a phone-log capture behind — the next probe's
# 12 s dwell assumes the previous one went quiet. Only an app THIS run launched is killed (a NOT RUN
# exit before any launch leaves the owner's app alone). A run that reached a verdict keeps the app
# up, as before (the next probe, or the owner, takes it from there); the capture is always stopped.
cleanup() {
  [ -n "$CAP" ] && kill "$CAP" 2>/dev/null
  pkill -f "pymobiledevice3 syslog" 2>/dev/null
  [ "$DONE" = 1 ] || [ "$LAUNCHED" = 0 ] || pkill -f "carlink-run.*carlink_macOS" 2>/dev/null
}
trap cleanup EXIT
case "$SPEC" in
  *:initial)
    echo "$LABEL  $SPEC => NOT RUN — ':initial' has no app equivalent and the box lever is ignored while the app's config is in force (app-less only; see the header)"
    exit 2 ;;
esac
RECT="$SPEC"
BID=zeno.carlink.mac
OUT=/tmp/valog/$LABEL.txt
APP_BIN=/tmp/carlink-run/Build/Products/Debug/carlink_macOS.app/Contents/MacOS/carlink_macOS
cd ~/Documents/carlink/ccpa_custom
mkdir -p /tmp/valog
ctl() { printf '%s\n' "$1" | nc -w 3 127.0.0.1 "$CTRL_PORT" 2>/dev/null; }
jf() { printf '%s' "$1" | python3 -c "import json,sys
try: v=json.load(sys.stdin)
except Exception: sys.exit(0)
for k in sys.argv[1].split('.'):
    v=v.get(k) if isinstance(v,dict) else None
print('' if v is None else (str(v).lower() if isinstance(v,bool) else v))" "$2" 2>/dev/null; }
# The observed rect as "WxH@X,Y" from ONE `get viewarea` reply (empty when not observed).
observed_rect() { ctl "get viewarea" | python3 -c 'import json,sys
try: v=json.load(sys.stdin)
except Exception: sys.exit(0)
a=v.get("viewArea") if v.get("observed") is True else None
if isinstance(a,dict): print("%sx%s@%s,%s"%(a.get("width"),a.get("height"),a.get("originX"),a.get("originY")))' 2>/dev/null; }
# settle_geometry / rect_close: MINIMAL COPIES of tools/va_wired_sweep.sh's (same names, same
# defaults; that file carries the device evidence for both — keep the two in step). Not factored
# into a shared file because the sweep's versions sit on its jq-based `jf`/`log` and are
# device-proven as written.
#   settle_geometry: iOS runs its OWN view-area round trip at session start (into area [1], ~2.4 s,
#     back to [0], ~8 s of geometry records); a `viewarea request` sent inside it is swallowed.
#     Wait until the VideoConfig record counter `changes` holds still.
#   rect_close: iOS can settle a rect 1 px off toward the origin; compare within RECT_TOL.
SETTLE_POLL="${SETTLE_POLL:-2}"; SETTLE_STABLE="${SETTLE_STABLE:-3}"; SETTLE_MAX="${SETTLE_MAX:-45}"
settle_geometry() {
  local last="" cur held=0 waited=0
  while [ "$waited" -lt "$SETTLE_MAX" ]; do
    cur=$(jf "$(ctl "get viewarea")" changes)
    if [ -n "$cur" ] && [ "$cur" = "$last" ]; then
      held=$((held + 1)); [ "$held" -ge "$SETTLE_STABLE" ] && return 0
    else
      held=0
    fi
    last="$cur"; sleep "$SETTLE_POLL"; waited=$((waited + SETTLE_POLL))
  done
  return 1
}
RECT_TOL="${RECT_TOL:-2}"
rect_close() { # observed armed -> 0 when every component is within RECT_TOL
  local a="$1" b="$2"
  [ -n "$a" ] && [ -n "$b" ] || return 1
  [ "$a" = "$b" ] && return 0
  printf '%s %s %s\n' "$a" "$b" "$RECT_TOL" | awk '
    { split($1, o, /[x@,]/); split($2, w, /[x@,]/); tol = $3 + 0
      for (i = 1; i <= 4; i++) { d = (o[i] + 0) - (w[i] + 0); if (d < 0) d = -d; if (d > tol) exit 1 }
      exit 0 }'
}
# This run's app-log slice from line $1, WITHOUT backfill: on connect the app replays the box's log
# history tagged `[backfill]`, and those lines describe the PREVIOUS session (va_wired_sweep.sh
# `live_log` has the device-proven case of a backfilled REFUSED mis-scoring a good run).
live_from() { tail -n +"$1" "$L" 2>/dev/null | grep -av "\[backfill\]"; }

launch_app() {
  LAUNCHED=1
  CARLINK_CTRL_PORT=$CTRL_PORT nohup "$APP_BIN" >/tmp/carlink-stdout.log 2>&1 &
  for _ in $(seq 1 30); do [ -n "$(ctl status)" ] && return 0; sleep 1; done
  echo "$LABEL  $SPEC => APP FAILED (control port $CTRL_PORT never answered — sandboxed shell?)"; exit 3
}
kill_app() {
  pkill -f "carlink-run.*carlink_macOS" 2>/dev/null
  # 12s, NOT 3s — see below (async supervisor teardown).
  sleep 12
}

pkill -f "carlink-run.*carlink_macOS" 2>/dev/null
pkill -f "pymobiledevice3 syslog" 2>/dev/null
# 12s, NOT 3s. The supervisor's wireless teardown is ASYNC: on 2026-09-05 its SIGTERM landed 6s
# after the app had already restarted, so the "CLEAN bring-up" that ran in between was killed by
# the PREVIOUS teardown's signal. The supervisor logs bring-up success either way, so the stack
# reads healthy while the advertiser is dead and no BT SSID appears — i.e. pairing silently stops
# working. Wait out the teardown before forcing the next host-PRESENT edge.
sleep 12

defaults write "$BID" vc.enablesViewAreas -bool true
defaults write "$BID" vc.enablesCornerMasks -bool "$CM"
defaults write "$BID" vc.enablesFocusTransfer -bool "$FT"
# Arm through the app door, then restart the app: the pushed config is consumed at SUBSCRIBE and
# carplayd reads it per connection, so a fresh app connection is what applies the saved rect.
launch_app
R=$(ctl "viewarea arm $RECT")
case "$R" in
  ""|"ERR unknown command"*)
    echo "$LABEL  $SPEC => NOT RUN — this app build has no 'viewarea arm' door ($R); rebuild the app (the box lever is ignored while its config is in force)"; exit 3 ;;
  ERR*)
    echo "$LABEL  $SPEC => REFUSED LOCALLY (door: $R)"; exit 0 ;;
esac
if [ "$(jf "$R" ok)" = "false" ] || [ "$(jf "$R" armed.legal)" = "false" ]; then
  echo "$LABEL  $SPEC => REFUSED LOCALLY (app rule: $(jf "$R" armed.verdict))"; exit 0
fi
S=$(ctl save)
[ "$(jf "$S" ok)" = "true" ] || { echo "$LABEL  $SPEC => REFUSED LOCALLY (save: $S)"; exit 0; }
kill_app

: > "$OUT"
# UNFILTERED capture, grep-narrowed on the way to disk. `-pn airplayd` was wrong: the
# "unsupported resolution" banner is rendered by CarPlay's UI process on the phone, NOT by
# airplayd, so a process filter hid the single most important signal. Filtering by CONTENT keeps
# every process in scope and still lands a small file.
nohup sh -c "pymobiledevice3 syslog live 2>/dev/null | grep -aiE \
  'view ?area|checkCarPlayFeatureAcceptance|isViewAreaPixelSizeAcceptable|minAcceptableViewArea|ScaleInfo|kFigEndpointError|InvalidParameter|does not support this display resolution|unsupported resolution|resolution|CarPlay ViewAreas' \
  > '$OUT'" >/dev/null 2>&1 &
CAP=$!
sleep 2

launch_app
sleep 6

VERDICT="INCONCLUSIVE (no declaration in 200s)"
teardown_since() { [ "$(live_from "$1" | grep -c "full TEARDOWN")" -gt 0 ]; }
box_refused_idx1() { live_from "$1" | grep -q "host viewArea index=1 REFUSED"; }
for i in $(seq 1 40); do
  L=$(ls -t ~/Library/Logs/Carlink/carlink_*.log | head -1)
  dl=$(grep -an "view areas: 2 declared" "$L" 2>/dev/null | grep -av "\[backfill\]" | grep -F "$RECT," | tail -1 | cut -d: -f1)
  [ -z "$dl" ] && dl=$(grep -an "REFUSED" "$L" 2>/dev/null | grep -av "\[backfill\]" | grep -F "$RECT" | tail -1 | cut -d: -f1)
  if [ -n "$dl" ]; then
    if live_from "$dl" | grep -v "host viewArea index=" | grep -q "REFUSED"; then VERDICT="REFUSED LOCALLY (never reached the phone)"; break; fi
    if teardown_since "$dl"; then VERDICT="REJECTED (teardown after declaration)"; break; fi
    obs=$(jf "$(ctl "get viewarea")" observed)
    if [ "$obs" = "true" ]; then
      VERDICT=""   # every path below assigns one before the break
      # The session starts in area [0] (no `:initial` through the app); ask for [1] so iOS actually
      # renders the probed rect — the old `:initial` spec started there. First let iOS's own
      # session-start resize finish (see settle_geometry), or the request is swallowed.
      settle_geometry || echo "  (geometry still moving after ${SETTLE_MAX}s — requesting anyway)" >&2
      if teardown_since "$dl"; then VERDICT="REJECTED (teardown after declaration)"; break; fi
      RQ=$(ctl "viewarea request 1")
      if [ "$(jf "$RQ" ok)" != "true" ]; then
        VERDICT="INCONCLUSIVE (viewarea request 1 not sent: ${RQ:-no reply})"; break
      fi
      # The rect must actually LAND: poll the observed rect, watching for a teardown and for the
      # box refusing index 1 (area [1] not declared this session, so the phone was never asked).
      t=0; CUR=""
      while [ "$t" -lt "$TRANSITION_TIMEOUT" ]; do
        if teardown_since "$dl"; then VERDICT="REJECTED (teardown after viewarea request 1)"; break; fi
        if box_refused_idx1 "$dl"; then VERDICT="INCONCLUSIVE (box REFUSED index 1 — area [1] not declared this session)"; break; fi
        CUR=$(observed_rect)
        rect_close "$CUR" "$RECT" && break
        sleep 1; t=$((t + 1))
      done
      [ -n "$VERDICT" ] && break
      if ! rect_close "$CUR" "$RECT"; then
        VERDICT="INCONCLUSIVE (rect stayed ${CUR:-unobserved} for ${TRANSITION_TIMEOUT}s after viewarea request 1)"; break
      fi
      sleep 10
      CUR=$(observed_rect)
      if teardown_since "$dl"; then VERDICT="REJECTED (teardown 10s after projecting)"
      elif box_refused_idx1 "$dl"; then VERDICT="INCONCLUSIVE (box REFUSED index 1 — area [1] not declared this session)"
      elif ! rect_close "$CUR" "$RECT"; then VERDICT="INCONCLUSIVE (rect landed, then moved to ${CUR:-unobserved} within 10s)"
      else VERDICT="ACCEPTED"; fi
      break
    fi
  fi
  sleep 5
done
DONE=1

sleep 3
kill "$CAP" 2>/dev/null; pkill -f "pymobiledevice3 syslog" 2>/dev/null; CAP=""
# ACCEPTED-but-degraded: iOS took the declaration, then the CarPlay UI refused to RENDER the area.
# Observed 2026-09-05 on 480x300 — no teardown, session healthy, on-screen "unsupported resolution".
# A declaration-only verdict calls that a pass, which is how the size floor stayed invisible.
if [ "$VERDICT" = "ACCEPTED" ] && grep -aqiE "does not support this display resolution" "$OUT"; then
  VERDICT="DEGRADED (declared + accepted, but the UI reports an unsupported resolution)"
fi
BYTES=$(wc -c < "$OUT" | tr -d ' ')
echo "$LABEL  $SPEC  cm=$CM ft=$FT  => $VERDICT   [phone log ${BYTES}B]"
echo "  -- phone reasons:"
grep -aiE "checkCarPlayFeatureAcceptance|isViewAreaPixelSizeAcceptable|minAcceptableViewArea|view ?area|ScaleInfo|kFigEndpointError|-16720|InvalidParameter" "$OUT" \
  | grep -aviE "^\s*$" | tail -12 | sed 's/^/     /'
