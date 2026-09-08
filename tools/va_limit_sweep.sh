#!/bin/bash
# Sweep view-area sizes from the product floor up to 4K, in both orientations.
#
# A view area MUST be contained in the panel (device-proven teardown otherwise), so testing a 4K
# AREA requires a 4K PANEL — the panel is driven here as well, via the same UserDefaults the app
# loads at launch. Origin is centred and forced EVEN: odd x/y/w/h is a device-proven session
# teardown (`carEndpoint_copyScreenInfo:7001` -> -16720, HEVC 4:2:0 cannot express an odd extent).
BID=zeno.carlink.mac
RESULTS=/tmp/valog/sweep_results.txt

run_one() { # panel_w panel_h area_w area_h label
  local PW=$1 PH=$2 AW=$3 AH=$4 LABEL=$5
  local X=$(( ((PW - AW) / 2) / 2 * 2 ))
  local Y=$(( ((PH - AH) / 2) / 2 * 2 ))
  defaults write "$BID" vc.mainWidth  -int "$PW"
  defaults write "$BID" vc.mainHeight -int "$PH"
  local out
  out=$(/tmp/valimits.sh "${AW}x${AH}@${X},${Y}:initial" "$LABEL" false false 2>&1 | grep -a "=>")
  printf 'panel %sx%s  area %sx%s  %s\n' "$PW" "$PH" "$AW" "$AH" "${out#*=> }" | tee -a "$RESULTS"
}

: > "$RESULTS"
echo "=== LANDSCAPE: panel 3840x2160, areas from the 800x480 floor up to 4K" | tee -a "$RESULTS"
for a in "800 480" "1280 720" "1920 1080" "2560 1440" "3840 2160"; do
  set -- $a; run_one 3840 2160 "$1" "$2" "L-$1x$2"
done

echo "=== PORTRAIT: panel 2160x3840, areas from the 480x800 floor up to 4K" | tee -a "$RESULTS"
for a in "480 800" "720 1280" "1080 1920" "1440 2560" "2160 3840"; do
  set -- $a; run_one 2160 3840 "$1" "$2" "P-$1x$2"
done

echo "=== DONE" | tee -a "$RESULTS"
