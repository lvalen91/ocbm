#!/bin/bash
# View-area limit prober: arm a spec, capture the PHONE's carplayd log across the attempt,
# classify, and extract the acceptance-check reason iOS gives when it refuses.
#
# The phone-side capture is the whole point: box logs say WHAT we declared, only the phone says WHY
# it refused. `-pn carplayd` is what makes this tractable — an unfiltered capture of the same window
# ran 4.4 GB and still missed the line.
# Requires WIRELESS CarPlay: the iPhone's USB port must be free to talk to this Mac.
SPEC="$1"; LABEL="$2"; CM="${3:-false}"; FT="${4:-false}"
# The box logs the RECT only — the ":initial" suffix is consumed by the parser and never printed —
# so match on the rect, not on the raw spec, or an ":initial" run is always INCONCLUSIVE.
RECT="${SPEC%%:initial}"
BID=zeno.carlink.mac
OUT=/tmp/valog/$LABEL.txt
cd ~/Documents/carlink/ccpa_custom

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
printf '%s' "$SPEC" > /tmp/va2spec
./target/release/ocbm-host push 1314 2d00 /tmp/va2spec /tmp/carplay_viewarea2 644 >/dev/null 2>&1 \
  || { echo "$LABEL  $SPEC => PUSH FAILED (box not on USB / app still holding it)"; exit 3; }

: > "$OUT"
# UNFILTERED capture, grep-narrowed on the way to disk. `-pn carplayd` was wrong: the
# "unsupported resolution" banner is rendered by CarPlay's UI process on the phone, NOT by
# carplayd, so a process filter hid the single most important signal. Filtering by CONTENT keeps
# every process in scope and still lands a small file.
nohup sh -c "pymobiledevice3 syslog live 2>/dev/null | grep -aiE \
  'view ?area|checkCarPlayFeatureAcceptance|isViewAreaPixelSizeAcceptable|minAcceptableViewArea|ScaleInfo|kFigEndpointError|InvalidParameter|does not support this display resolution|unsupported resolution|resolution|CarPlay ViewAreas' \
  > '$OUT'" >/dev/null 2>&1 &
CAP=$!
sleep 2

CARLINK_CTRL_PORT=8765 nohup /tmp/carlink-run/Build/Products/Debug/carlink_macOS.app/Contents/MacOS/carlink_macOS >/tmp/carlink-stdout.log 2>&1 &
sleep 6

VERDICT="INCONCLUSIVE (no declaration in 200s)"
for i in $(seq 1 40); do
  L=$(ls -t ~/Library/Logs/Carlink/carlink_*.log | head -1)
  dl=$(grep -n "view areas: 2 declared" "$L" 2>/dev/null | grep -F "$RECT," | tail -1 | cut -d: -f1)
  [ -z "$dl" ] && dl=$(grep -n "REFUSED" "$L" 2>/dev/null | grep -F "$RECT" | tail -1 | cut -d: -f1)
  if [ -n "$dl" ]; then
    if tail -n +"$dl" "$L" | grep -q "REFUSED"; then VERDICT="REFUSED LOCALLY (never reached the phone)"; break; fi
    td=$(tail -n +"$dl" "$L" | grep -c "full TEARDOWN")
    obs=$(printf 'get viewarea\n' | nc -w 2 127.0.0.1 8765 2>/dev/null | python3 -c "import json,sys;print(json.load(sys.stdin).get('observed'))" 2>/dev/null)
    if [ "${td:-0}" -gt 0 ]; then VERDICT="REJECTED (teardown after declaration)"; break; fi
    if [ "$obs" = "True" ]; then
      sleep 10
      td2=$(tail -n +"$dl" "$L" | grep -c "full TEARDOWN")
      if [ "${td2:-0}" -gt 0 ]; then VERDICT="REJECTED (teardown 10s after projecting)"; else VERDICT="ACCEPTED"; fi
      break
    fi
  fi
  sleep 5
done

sleep 3
kill "$CAP" 2>/dev/null; pkill -f "pymobiledevice3 syslog" 2>/dev/null
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
