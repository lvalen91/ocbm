#!/bin/bash
# va_wired_sweep.sh — the WIRED-CarPlay view-area resize matrix, judged from Mac-side signals only.
#
#   tools/va_wired_sweep.sh                      # the whole matrix (control case first)
#   tools/va_wired_sweep.sh --list               # print the matrix and exit
#   tools/va_wired_sweep.sh --only L-1280x720    # one label (repeatable, comma-separated)
#   tools/va_wired_sweep.sh --case 3840 2160 1280 720   # one ad-hoc case: panelW panelH areaW areaH
#   TAP="7400 9300" tools/va_wired_sweep.sh ...  # Dock resize-button tap (0..10000 space) — UNPINNED
#
# MUST run from a NON-sandboxed shell (see launch_app below). Kills and relaunches the app per case.
#
# WHY THIS EXISTS, SEPARATE FROM tools/va_limit_probe.sh / va_limit_sweep.sh. Those classify a run
# largely from the PHONE's log (pymobiledevice3 over the iPhone's USB port). In a WIRED session the
# iPhone's only port is plugged into the box (docs/ops/02_TESTING.md "The trace persists"), so THERE IS
# NO LIVE PHONE-SIDE CAPTURE IN THIS ARM — it is absent by construction, not dropped. Every verdict
# here comes from: the app's control socket (127.0.0.1:CTRL_PORT), the app's own log
# (~/Library/Logs/Carlink/carlink_*.log, which carries the box log stream), and PIXELS of the
# decoded frame (`shot`) or of the app window (tools/winshot.swift, by CGWindowID — never a screen
# region, which captures whatever window happens to be on top). iOS's own reason for a refusal is
# only recoverable retroactively (unplug the phone, plug it into the Mac, pull the archive — 02_TESTING).
#
# The two traps the wireless harness documents, carried forward so they are not reintroduced:
#   1. `-pn airplayd` phone-log filter hid the lockout banner (rendered by CarPlay's UI process). Not
#      applicable here — there is no phone log at all — which is why the LOCKOUT verdict is taken
#      from the pixels of the armed rect instead.
#   2. The app-restart dwell must be 12 s, not 3: the supervisor's teardown SIGTERM lands ~6 s after
#      the app is gone and kills a bring-up started before it. Wired teardown is `kill_session` on
#      the host-GONE edge (session_supervisor.sh::teardown); the dwell is kept for the same reason.
#
# CLASSIFICATION (per case, in this order):
#   REFUSED-LOCALLY  the app door (`viewarea arm`/`save`) or the app log ("REFUSED") rejected the
#                    rect before it reached the phone.
#   TEARDOWN         "full TEARDOWN" in the app log after the declaration, or sessionActive:false
#                    at any point after the session was up.
#   INCONCLUSIVE     session never reached sessionActive+observed in SESSION_TIMEOUT, or the
#                    observed rect never became the armed rect in TRANSITION_TIMEOUT after the
#                    trigger, or no trigger was available (no `viewarea request`, TAP unset).
#   LOCKOUT          session alive, observed rect == armed rect, but the armed rect's pixels are
#                    black: meanLuma < BLACK_MEAN and darkFrac > BLACK_DARK (iOS's "CarPlay does not
#                    support this display resolution" banner is white text on black — it lowers
#                    darkFrac a little, it does not lift the mean). No OCR is attempted.
#   ACCEPTED         session alive, observed rect == armed rect, pixels carry content.
# Every PNG and the socket replies land under $OUT/<label>/ so any verdict can be audited by eye.
#
# Matrix and centring arithmetic are those of tools/va_limit_sweep.sh: origin centred and forced
# EVEN (odd x/y/w/h is a device-proven teardown, `carEndpoint_copyScreenInfo:7001` -> -16720).
# The 800x480-in-3840x2160 case is already owner-confirmed and stays first as the control.

set -u
cd "$(dirname "$0")/.." || exit 2
REPO=$(pwd)

# ---- parameters (env-overridable) -------------------------------------------------------------
CTRL_PORT=${CTRL_PORT:-8765}
APP_BIN=${APP_BIN:-/tmp/carlink-run/Build/Products/Debug/carlink_macOS.app/Contents/MacOS/carlink_macOS}
BID=${BID:-zeno.carlink.mac}
OUT=${OUT:-/tmp/valog/wired}
RESULTS=${RESULTS:-$OUT/results.txt}
TAP=${TAP:-}                    # "x y" in the app's 0..10000 touch space — the Dock resize button. NOT YET PINNED.
DWELL=${DWELL:-12}              # app-restart dwell, trap 2 above
SESSION_TIMEOUT=${SESSION_TIMEOUT:-180}
TRANSITION_TIMEOUT=${TRANSITION_TIMEOUT:-30}
SETTLE=${SETTLE:-4}             # seconds after the rect lands, for the resize animation to finish
BLACK_MEAN=${BLACK_MEAN:-12}    # LOCKOUT thresholds, see CLASSIFICATION
BLACK_DARK=${BLACK_DARK:-0.85}
WIN_TOP=${WIN_TOP:-96}          # px of title bar in a winshot capture (48 pt at 2x on this display)
NO_RESTART=${NO_RESTART:-0}     # 1 = arm through the door but do not restart the app (owner applies)
INITIAL=${INITIAL:-0}           # 1 = append :initial to the BOX-LEVER spec (door path never authors it)
KEEP=${KEEP:-0}                 # 1 = leave the last rect armed at the end
WINSHOT="$REPO/tools/winshot.sh"
OCBM_HOST="$REPO/target/release/ocbm-host"

# ---- matrix -------------------------------------------------------------------------------------
MATRIX=(
  "3840 2160 800 480 L-800x480"        # control: owner-confirmed on 2026-09-05 (wireless)
  "3840 2160 1280 720 L-1280x720"
  "3840 2160 1920 1080 L-1920x1080"
  "3840 2160 2560 1440 L-2560x1440"
  "3840 2160 3840 2160 L-3840x2160"
  "2160 3840 480 800 P-480x800"
  "2160 3840 720 1280 P-720x1280"
  "2160 3840 1080 1920 P-1080x1920"
  "2160 3840 1440 2560 P-1440x2560"
  "2160 3840 2160 3840 P-2160x3840"
)

ONLY=""; ADHOC=""
while [ $# -gt 0 ]; do
  case "$1" in
    --list) printf '%s\n' "${MATRIX[@]}"; exit 0 ;;
    --only) ONLY="$2"; shift ;;
    --case) ADHOC="$2 $3 $4 $5 ADHOC-$4x$5-in-$2x$3"; shift 4 ;;
    -h|--help) sed -n '2,12p' "$0"; exit 0 ;;
    *) echo "unknown arg $1" >&2; exit 2 ;;
  esac
  shift
done

# ---- primitives ---------------------------------------------------------------------------------
LOGF=""
log() { printf '[%s] %s\n' "$(date +%H:%M:%S)" "$*" | tee -a "$LOGF" >&2; }

# One command, one reply line. nc closes after stdin EOF + the reply (measured ~5 ms), so -w only
# bounds a dead app. Empty reply == app not listening.
ctl() { printf '%s\n' "$1" | nc -w 3 127.0.0.1 "$CTRL_PORT" 2>/dev/null; }
# `ctl` + record the reply under the case dir so an audit sees exactly what the app said.
ctlr() { local r; r=$(ctl "$1"); printf '%s -> %s\n' "$1" "$r" >> "$CASEDIR/socket.txt"; printf '%s' "$r"; }
is_err() { case "$1" in ERR*|"") return 0 ;; *) return 1 ;; esac; }
jf() { printf '%s' "$1" | jq -r "$2" 2>/dev/null; }          # JSON field, "" on failure
app_pid() { pgrep -f "carlink-run.*carlink_macOS" | head -1; }
newest_log() { ls -t ~/Library/Logs/Carlink/carlink_*.log 2>/dev/null | head -1; }
L=""; LBASE=1
since_log() { tail -n +"$LBASE" "$L" 2>/dev/null; }   # this case's slice of the app log
# This case's slice with BACKFILL REMOVED — use this for anything that decides a verdict.
#
# On connect the app replays the box's log history, tagging each replayed line `[backfill]`. Those
# lines describe the PREVIOUS session. Device-proven cost, 2026-09-07: P-720x1280 declared its area
# correctly ([0] 2160x3840@0,0, [1] 720x1280@720,1280) and came up on the full panel, but a
# backfilled "host viewArea index=1 REFUSED — only 1 area(s) declared" from the case before it was
# still sitting in the same log file, so the case scored INCONCLUSIVE on another run's failure.
# Same shape of error as the panel clamp: the case looked measured and was not.
live_log() { since_log | grep -av "\[backfill\]"; }
# The observed rect as "WxH@X,Y" (empty when not observed).
observed_rect() {
  local v; v=$(ctl "get viewarea")
  [ "$(jf "$v" .observed)" = "true" ] || return 0
  jf "$v" '"\(.viewArea.width)x\(.viewArea.height)@\(.viewArea.originX),\(.viewArea.originY)"'
}
coded_size() { jf "$(ctl "get viewarea")" '"\(.coded.width)x\(.coded.height)"'; }

# Does the observed rect match the armed one, within RECT_TOL px on every component?
#
# NOT string equality, and this is device-proven, not defensive padding: on 2026-09-07, wired, panel
# 3840x2160, the armed 1920x1080@1900,1060 SETTLED at 1921x1081@1899,1059 and stayed there for 24 s
# (polled every 2 s, geometry-record counter frozen at 154 — the animation had finished). iOS
# converged one pixel out toward the origin and stopped; the far edges landed exactly (1899+1921 =
# 1900+1920 = 3820, 1059+1081 = 1060+1080 = 2140). Three other rects the same session — 1280x720@200,180,
# 800x480@3040,1680 and 2560x1440@640,360 — landed byte-exact, so this is a per-rect rounding artifact
# of the transition's final interpolation step, NOT a general offset that could be corrected for.
# Exact comparison hangs such a case until TRANSITION_TIMEOUT and then scores a good rect NO-MOVE.
# Note the settled rect is ODD here while the DECLARED one was even: the parity rule binds what we
# declare (docs/carplay/06_AV_PIPELINE.md), not what iOS renders back.
# Block until the geometry stops moving, BEFORE capturing or triggering anything.
#
# Device-proven necessity (2026-09-07): iOS runs its OWN view-area round trip at session start —
# unprompted, it shrinks into area [1], holds ~2.4 s, and grows back to [0], ~290 geometry records
# over ~8 s. The first run of this harness triggered in the same second the session came up: the
# press landed inside that animation, was swallowed, and the case scored INCONCLUSIVE with the rect
# never leaving 3840x2160@0,0. A BEFORE frame captured mid-animation is equally worthless — it is a
# picture of a rect nobody asked for. So: poll `changes` (the VideoConfig record counter) and only
# proceed once it has held still for SETTLE_STABLE consecutive samples.
SETTLE_POLL="${SETTLE_POLL:-2}"      # seconds between samples
SETTLE_STABLE="${SETTLE_STABLE:-3}"  # consecutive unchanged samples required
SETTLE_MAX="${SETTLE_MAX:-45}"       # give up and proceed anyway after this long
settle_geometry() {
  local last="" cur held=0 waited=0
  while [ "$waited" -lt "$SETTLE_MAX" ]; do
    cur=$(jf "$(ctl "get viewarea")" .changes)
    if [ -n "$cur" ] && [ "$cur" = "$last" ]; then
      held=$((held + 1))
      [ "$held" -ge "$SETTLE_STABLE" ] && { log "geometry settled after ${waited}s (changes=$cur)"; return 0; }
    else
      held=0
    fi
    last="$cur"; sleep "$SETTLE_POLL"; waited=$((waited + SETTLE_POLL))
  done
  log "geometry still moving after ${SETTLE_MAX}s (changes=$last) — proceeding anyway"
}

RECT_TOL="${RECT_TOL:-2}"
rect_close() { # observed armed -> 0 when every component is within RECT_TOL
  local a="$1" b="$2"
  [ -n "$a" ] && [ -n "$b" ] || return 1
  [ "$a" = "$b" ] && return 0
  printf '%s %s %s\n' "$a" "$b" "$RECT_TOL" | awk '
    function num(s, r) { r = s + 0; return r }
    {
      split($1, o, /[x@,]/); split($2, w, /[x@,]/); tol = $3 + 0
      for (i = 1; i <= 4; i++) { d = num(o[i]) - num(w[i]); if (d < 0) d = -d; if (d > tol) exit 1 }
      exit 0
    }'
}

kill_app() {
  pkill -f "carlink-run.*carlink_macOS" 2>/dev/null && log "app killed; dwell ${DWELL}s (trap 2: async supervisor teardown)"
  sleep "$DWELL"
}

launch_app() {
  # SANDBOX TRAP (owner, 2026-09-07): launched from a sandboxed shell (e.g. an agent's sandbox-exec'd
  # Bash) the app comes up, then dies a few seconds later with
  #   sandbox_extension_issue_file_to_process failed ... Operation not permitted
  # This exact line works from a plain Terminal — the difference is the CALLER's sandbox, not the
  # command. So this whole script must run outside any sandbox; the liveness check below turns the
  # trap into a hard error instead of a silent INCONCLUSIVE streak.
  CARLINK_CTRL_PORT="$CTRL_PORT" nohup "$APP_BIN" >/tmp/carlink-stdout.log 2>&1 &
  sleep 8
  if [ -z "$(app_pid)" ]; then
    log "FATAL: app died within 8 s of launch — if you are in a sandboxed shell, that is the trap:" \
        "run this script from a plain Terminal. Tail of /tmp/carlink-stdout.log follows."
    tail -5 /tmp/carlink-stdout.log >&2
    exit 3
  fi
  local i
  for i in $(seq 1 30); do [ -n "$(ctl status)" ] && return 0; sleep 1; done
  log "FATAL: app alive but 127.0.0.1:$CTRL_PORT never answered (CARLINK_CTRL_PORT not honoured?)"
  exit 3
}

ensure_app() { [ -n "$(ctl status)" ] || { log "app not listening — launching"; launch_app; }; }

# Fallback lever, app-less: the spec file on the box. `ocbm-host push` claims the same USB device
# the app holds, so the app MUST be down first — this is the ONLY place the script touches USB.
# /tmp on the box is tmpfs: the file persists across app restarts until the box reboots, and this
# script cannot remove it without USB, so a later door-path case runs with the lever still present.
lever_arm() { # PW PH RECT
  kill_app
  defaults write "$BID" vc.mainWidth -int "$1"
  defaults write "$BID" vc.mainHeight -int "$2"
  defaults write "$BID" vc.enablesViewAreas -bool true
  local spec="$3"; [ "$INITIAL" = 1 ] && spec="$spec:initial"
  printf '%s' "$spec" > "$CASEDIR/va2spec"
  if ! "$OCBM_HOST" push 1314 2d00 "$CASEDIR/va2spec" /tmp/carplay_viewarea2 644 >"$CASEDIR/push.log" 2>&1; then
    log "lever push FAILED (box not on USB?) — see $CASEDIR/push.log"; return 1
  fi
  log "lever armed: /tmp/carplay_viewarea2 = '$spec'"
  launch_app
}

frame_stats() { # png rect src [top] -> "mean dark" (empty on failure)
  local top=${4:-0}
  "$WINSHOT" stats "$1" --rect "$2" --src "$3" --top "$top" 2>/dev/null \
    | jq -r '[.meanLuma, .darkFrac] | @tsv' 2>/dev/null | awk '{printf "%.1f %.3f", $1, $2}'
}
frame_diff() { # before after rect src [top] -> "meanAbsDiff/changedFrac" (empty on failure)
  local top=${5:-0}
  "$WINSHOT" diff "$1" "$2" --rect "$3" --src "$4" --top "$top" 2>/dev/null \
    | jq -r '[.meanAbsDiff, .changedFrac] | @tsv' 2>/dev/null | awk '{printf "%.1f/%.3f", $1, $2}'
}

# ---- one case -----------------------------------------------------------------------------------
run_one() { # PW PH AW AH LABEL
  local PW=$1 PH=$2 AW=$3 AH=$4 LABEL=$5
  local X=$(( ((PW - AW) / 2) / 2 * 2 ))
  local Y=$(( ((PH - AH) / 2) / 2 * 2 ))
  local RECT="${AW}x${AH}@${X},${Y}"
  CASEDIR="$OUT/$LABEL"; mkdir -p "$CASEDIR"; : > "$CASEDIR/socket.txt"
  LOGF="$CASEDIR/run.log"
  local VERDICT="" NOTE="" ARM="app-door" TRIGGER="none" RECT0="" RECT1="" STATS="" DIFF=""
  log "=== $LABEL  panel ${PW}x${PH}  area $RECT"

  # a. arm ---------------------------------------------------------------------------------------
  ensure_app
  local r
  r=$(ctlr "set width $PW");  is_err "$r" && log "set width: $r"
  r=$(ctlr "set height $PH"); is_err "$r" && log "set height: $r"
  r=$(ctlr "viewarea arm $RECT")
  if [ -z "$r" ] || printf '%s' "$r" | grep -q "^ERR unknown command"; then
    # The VERB is missing (older app build) — only then is the box lever the right fallback.
    ARM="box-lever"
    log "app door has no 'viewarea arm' ($r) — falling back to the box lever"
    lever_arm "$PW" "$PH" "$RECT" || { VERDICT="INCONCLUSIVE"; NOTE="lever push failed"; }
  elif is_err "$r"; then
    # Any other ERR is the door rejecting THIS spec (parse/usage) — a local refusal, not a fallback.
    VERDICT="REFUSED-LOCALLY"; NOTE="door: $r"
  else
    log "door: $r"
    # PANEL GUARD. The door answers ok:true for a dimension it silently CLAMPED, and the reply's
    # `armed.panel` is what the model actually holds — so compare it against what this case asked
    # for. Without this, 2026-09-07: `set height 3840` was clamped to 2160 by a landscape-shaped
    # envelope (SettingsWindow minHeight/maxHeight applied per axis), every portrait area then failed
    # containment against a panel squared off to 2160x2160, the emitter dropped area [1], and all
    # five portrait cases scored INCONCLUSIVE — while the landscape half passed 5/5 and the run
    # looked healthy. A case that did not get the panel it asked for has measured NOTHING; say so.
    local gotW gotH
    gotW=$(jf "$r" '.armed.panel.width // empty'); gotH=$(jf "$r" '.armed.panel.height // empty')
    if [ -n "$gotW" ] && [ -n "$gotH" ] && { [ "$gotW" != "$PW" ] || [ "$gotH" != "$PH" ]; }; then
      VERDICT="REFUSED-LOCALLY"
      NOTE="panel clamped by the app: asked ${PW}x${PH}, model holds ${gotW}x${gotH} — case measures nothing"
      log "PANEL GUARD: $NOTE"
    fi
  fi
  if [ -z "$VERDICT" ] && [ "$ARM" != "box-lever" ]; then
    # Reply shape (ControlServer.viewarea, 2026-09-07): {"ok":bool,"armed":{"legal":bool,
    # "verdict":string|null,"active":bool,...},"dirty":bool}. `legal:false` carries the same
    # ViewArea2Rule message the Settings form shows — that is the app refusing before the phone.
    local ok legal verdict
    ok=$(jf "$r" .ok); legal=$(jf "$r" '.armed.legal // empty'); verdict=$(jf "$r" '.armed.verdict // .reason // empty')
    if [ "$ok" = "false" ] || [ "$legal" = "false" ]; then
      VERDICT="REFUSED-LOCALLY"; NOTE="door: ${verdict:-$r}"
    else
      r=$(ctlr "save")
      if [ "$(jf "$r" .ok)" != "true" ]; then VERDICT="REFUSED-LOCALLY"; NOTE="save: $r"
      elif [ "$NO_RESTART" = 1 ]; then log "NO_RESTART=1: config lands at the next SUBSCRIBE — apply it yourself"
      else
        # Pushed config is consumed at SUBSCRIBE, and a wired airplayd reads it per connection —
        # so a fresh app connection (host GONE -> teardown -> host PRESENT -> arm) is what applies it.
        kill_app; launch_app
      fi
    fi
  fi

  # b. session up ---------------------------------------------------------------------------------
  local decl=0 t=0
  if [ -z "$VERDICT" ]; then
    L=$(newest_log)
    # Only lines written THIS case count: the app log is per launch, but with NO_RESTART=1 it is
    # the owner's running log and an old "full TEARDOWN" must not be read as this case's.
    if [ "$NO_RESTART" = 1 ]; then LBASE=$(( $(wc -l < "$L") + 1 )); else LBASE=1; fi
    log "waiting for sessionActive+observed (<= ${SESSION_TIMEOUT}s), log $L from line $LBASE"
    while [ "$t" -lt "$SESSION_TIMEOUT" ]; do
      if live_log | grep "REFUSED" | grep -qF "$RECT"; then
        VERDICT="REFUSED-LOCALLY"; NOTE="app log REFUSED"; break; fi
      if [ "$decl" = 0 ] && since_log | grep -F "view areas: 2 declared" | grep -qF "[1] $RECT"; then
        decl=1; log "declared: $(since_log | grep -F "view areas: 2 declared" | grep -F "[1] $RECT" | tail -1 | sed 's/.*\[airplayd\] //' | cut -c1-120)"; fi
      if [ "$decl" = 1 ] && live_log | grep -q "full TEARDOWN"; then VERDICT="TEARDOWN"; NOTE="teardown after declaration"; break; fi
      local sess; sess=$(ctl "get session")
      if [ "$(jf "$sess" .sessionActive)" = "true" ]; then
        RECT0=$(observed_rect)
        [ -n "$RECT0" ] && break
      fi
      sleep 3; t=$((t + 3))
    done
    if [ -z "$VERDICT" ] && [ -z "$RECT0" ]; then VERDICT="INCONCLUSIVE"; NOTE="no session in ${SESSION_TIMEOUT}s"; fi
    [ "$decl" = 0 ] && [ -z "$VERDICT" ] && NOTE="declaration line for [1] $RECT not seen in app log"
  fi

  # c. BEFORE -------------------------------------------------------------------------------------
  local CODED="" shot_ok=0
  if [ -z "$VERDICT" ]; then
    settle_geometry
    RECT0=$(observed_rect)
    CODED=$(coded_size)
    log "session up: observed $RECT0, coded $CODED — capturing BEFORE"
    r=$(ctlr "shot $CASEDIR/before_frame.png")
    if [ "$(jf "$r" .ok)" = "true" ]; then shot_ok=1; log "shot: $r"; else log "shot unavailable ($r) — window capture only"; fi
    "$WINSHOT" shot "$(app_pid)" "$CASEDIR/before_win.png" >> "$CASEDIR/socket.txt" 2>&1 || log "winshot BEFORE failed"
  fi

  # d. trigger ------------------------------------------------------------------------------------
  if [ -z "$VERDICT" ]; then
    # TRIGGER=auto|request|tap. `auto` prefers the deterministic door and falls back to the Dock
    # button. Force `tap` when the APP has the door but the BOX has not been updated: the app answers
    # {"ok":true} as soon as it has SENT CMD_VIEW_AREA 0x11, and an airplayd that predates that opcode
    # logs "unknown INPUT_COMMAND 0x11 — dropped". The trigger then looks accepted, nothing moves, and
    # every case scores INCONCLUSIVE at TRANSITION_TIMEOUT instead of falling back here.
    if [ "${TRIGGER_MODE:-auto}" = "tap" ]; then r='{"ok":false,"reason":"TRIGGER_MODE=tap"}'
    else r=$(ctlr "viewarea request 1"); fi
    # {"ok":true,...} = accepted for send. ERR (verb missing) or {"ok":false,"reason":..} = no
    # deterministic trigger; fall back to the Dock button if its coordinates are pinned.
    if [ "$(jf "$r" .ok)" = "true" ]; then TRIGGER="request"; log "trigger: viewarea request 1 -> $r"
    elif [ -n "$TAP" ]; then
      log "viewarea request unavailable ($r) — tapping"
      # Dock resize button. Coordinates are the app's 0..10000 normalised space over the WHOLE
      # panel (AppDelegate.injectTouch -> didMultiTouch x/10000): pin them from a `shot` PNG as
      # px / codedW * 10000, py / codedH * 10000.
      # shellcheck disable=SC2086
      r=$(ctlr "tap $TAP"); TRIGGER="tap"; log "trigger: tap $TAP -> $r"
    else
      VERDICT="INCONCLUSIVE"; NOTE="no trigger: 'viewarea request' unavailable ($r) and TAP is unset"
    fi
  fi

  # e. wait for the rect, capture AFTER, classify --------------------------------------------------
  if [ -z "$VERDICT" ]; then
    t=0; RECT1="$RECT0"
    while [ "$t" -lt "$TRANSITION_TIMEOUT" ]; do
      if live_log | grep -q "full TEARDOWN" || [ "$(jf "$(ctl "get session")" .sessionActive)" != "true" ]; then
        VERDICT="TEARDOWN"; NOTE="after trigger ($TRIGGER)"; break; fi
      # The box refuses an updateViewArea for an index it never declared — the rect did not land in
      # the declaration (lever/door mismatch), so the phone was never asked.
      if live_log | grep -q "host viewArea index=1 REFUSED"; then
        VERDICT="INCONCLUSIVE"; NOTE="box REFUSED index 1 — area [1] not declared this session"; break; fi
      RECT1=$(observed_rect)
      rect_close "$RECT1" "$RECT" && break
      sleep 1; t=$((t + 1))
    done
    if [ -z "$VERDICT" ]; then
      sleep "$SETTLE"
      r=$(ctlr "shot $CASEDIR/after_frame.png"); [ "$(jf "$r" .ok)" = "true" ] || shot_ok=0
      local age; age=$(jf "$r" .frameAgeMs); [ -n "$age" ] && [ "$age" != null ] && [ "$age" -gt 2000 ] 2>/dev/null && NOTE="$NOTE; AFTER frame ${age}ms old"
      "$WINSHOT" shot "$(app_pid)" "$CASEDIR/after_win.png" >> "$CASEDIR/socket.txt" 2>&1 || log "winshot AFTER failed"
      # Re-check liveness once more after the settle — a late teardown is still a teardown.
      if live_log | grep -q "full TEARDOWN" || [ "$(jf "$(ctl "get session")" .sessionActive)" != "true" ]; then
        VERDICT="TEARDOWN"; NOTE="within ${SETTLE}s of the rect landing"
      elif ! rect_close "$RECT1" "$RECT"; then
        VERDICT="INCONCLUSIVE"; NOTE="rect stayed $RECT1 for ${TRANSITION_TIMEOUT}s after $TRIGGER"
      else
        # Pixels of the armed rect. Prefer the decoded frame (1:1 with the coded panel); fall back
        # to the window capture (title bar skipped, rect mapped proportionally onto the content).
        local src="$CODED" a="$CASEDIR/after_frame.png" b="$CASEDIR/before_frame.png" top=0
        if [ "$shot_ok" = 0 ] || [ ! -s "$a" ]; then
          a="$CASEDIR/after_win.png"; b="$CASEDIR/before_win.png"; top=$WIN_TOP; src="${PW}x${PH}"
        fi
        STATS=$(frame_stats "$a" "$RECT" "$src" "$top")
        local mean=${STATS%% *} dark=${STATS##* }
        [ -s "$b" ] && DIFF=$(frame_diff "$b" "$a" "$RECT" "$src" "$top")
        if [ -z "$mean" ]; then VERDICT="INCONCLUSIVE"; NOTE="rect landed but no capture to score"
        elif awk -v m="$mean" -v d="$dark" -v M="$BLACK_MEAN" -v D="$BLACK_DARK" 'BEGIN{exit !(m < M && d > D)}'; then
          VERDICT="LOCKOUT"; NOTE="rect black: mean $mean dark $dark (banner not OCR'd — check after_*.png)"
        else
          VERDICT="ACCEPTED"; NOTE="mean $mean dark $dark"
          [ "${DIFF#*/}" = "0.000" ] && NOTE="$NOTE; frame unchanged vs BEFORE — verify by eye"
        fi
      fi
    fi
  fi

  local line
  line=$(printf 'panel %sx%s  area %-20s arm=%-9s trigger=%-7s before=%-18s after=%-18s luma=%-14s diff=%-10s => %s%s' \
    "$PW" "$PH" "$RECT" "$ARM" "$TRIGGER" "${RECT0:--}" "${RECT1:--}" "${STATS:--}" "${DIFF:--}" "$VERDICT" "${NOTE:+  ($NOTE)}")
  echo "$LABEL  $line" | tee -a "$RESULTS"
}

# ---- main ---------------------------------------------------------------------------------------
mkdir -p "$OUT"
[ -x "$OCBM_HOST" ] || echo "note: $OCBM_HOST missing — the box-lever fallback is unavailable" >&2
{ echo "=== va_wired_sweep $(date '+%Y-%m-%d %H:%M:%S')  TAP='${TAP:-unset}'  NO_RESTART=$NO_RESTART"; } | tee -a "$RESULTS"
if [ -n "$ADHOC" ]; then
  # shellcheck disable=SC2086
  run_one $ADHOC
else
  for row in "${MATRIX[@]}"; do
    # shellcheck disable=SC2086
    set -- $row
    if [ -n "$ONLY" ] && ! printf ',%s,' "$ONLY" | grep -qF ",$5,"; then continue; fi
    run_one "$@"
  done
fi
if [ "$KEEP" != 1 ] && [ -n "$(ctl status)" ]; then
  r=$(ctl "viewarea off"); is_err "$r" || { ctl save >/dev/null; echo "disarmed via door (lands at the next SUBSCRIBE)"; }
fi
echo "=== DONE — results: $RESULTS ; captures under $OUT/<label>/" | tee -a "$RESULTS"
