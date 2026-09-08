#!/bin/bash
# Capture all three sides of a CarPlay session attempt at once, into one timestamped directory.
#
#   iPhone     — os_log via pymobiledevice3 (idevicesyslog does NOT capture os_log; do not substitute it)
#   Head unit  — adb logcat, tag NETPROBE (the app) plus the system's own CarPlay/Wi-Fi chatter
#   Adapter    — /tmp/wl.log over the UART root console, snapshotted at the end
#
# The iPhone side is the one that actually explains failures: iOS names the parameter and the reason
# it rejected an accessory, which is far cheaper than bisecting from our side. That is how the
# cornerMask placement and the Ultra certificate gate were both cracked upstream.
#
# Usage: tools/tri_capture.sh [seconds]        (default 90)
# Repo-relative roots. This project lives at ccpa_custom/host/gm_ccpa, so the tools resolve both
# roots from their own location rather than hard-coding an absolute path — moving the checkout, or
# having a second one, must not silently build against the wrong tree.
GM_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")/.." && pwd)"
CCPA_ROOT="$(cd "$GM_ROOT/../.." && pwd)"

set -uo pipefail

SECS="${1:-90}"
CCPA="$CCPA_ROOT"
OUT="$GM_ROOT/evidence/tri_$(date +%Y%m%d-%H%M%S)"
mkdir -p "$OUT"
echo "[tri] capturing ${SECS}s into $OUT"
# Ctrl-C during the sleep would otherwise orphan both streams.
trap 'kill $HU ${IOS:-} 2>/dev/null' EXIT INT TERM

# ---- head unit -------------------------------------------------------------------------------
# Do NOT clear the buffer here. The USB_DEVICE_ATTACHED launch means the adapter attach, the
# System.loadLibrary result and often the first pairing attempt are ALREADY in the ring by the time
# an operator reaches a terminal — `logcat -c` wipes exactly the lines the capture exists for (lost
# the 2026-08-12 handshake that way). Mark the start instead and keep the history.
# Record the head-unit clock skew FIRST. This unit's clock is ~386 days behind real time, so
# logcat stamps and iPhone os_log stamps cannot be compared without it. Add OFFSET to a headunit
# stamp to get real/iPhone time.
{ echo "mac_utc=$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
  echo "mac_epoch=$(date +%s)"
  echo "headunit_local=$(adb shell 'date' 2>/dev/null | tr -d '\r')"
  echo "headunit_epoch=$(adb shell 'date +%s' 2>/dev/null | tr -d '\r')"
  echo "offset_headunit_minus_mac_s=$(( $(adb shell 'date +%s' 2>/dev/null | tr -d '\r') - $(date +%s) ))"
} > "$OUT/clock_offset.txt" 2>&1
cat "$OUT/clock_offset.txt"

adb shell log -t NETPROBE "=== tri_capture start ===" 2>/dev/null || true
adb logcat -v time > "$OUT/headunit_full.log" 2>&1 & HU=$!
echo "[tri] head unit: adb logcat -> headunit_full.log (pid $HU)"

# ---- iPhone ----------------------------------------------------------------------------------
IOS=""
if pymobiledevice3 usbmux list 2>/dev/null | grep -q Identifier; then
    pymobiledevice3 syslog live > "$OUT/iphone_syslog.log" 2>&1 & IOS=$!
    echo "[tri] iPhone: pymobiledevice3 syslog live -> iphone_syslog.log (pid $IOS)"
else
    echo "[tri] iPhone: NOT VISIBLE over USB — skipping."
    echo "      Check the cable carries data and that 'Trust This Computer' was accepted."
    echo "      For unredacted CarPlay logging, install Apple's CarPlay/AirPlay logging profile:"
    echo "      https://developer.apple.com/bug-reporting/profiles-and-logs/"
fi

sleep "$SECS"

# ---- stop the streams ------------------------------------------------------------------------
kill "$HU" 2>/dev/null; wait "$HU" 2>/dev/null
[ -n "$IOS" ] && { kill "$IOS" 2>/dev/null; wait "$IOS" 2>/dev/null; }
sleep 1

# ---- adapter (snapshot; the UART console is single-holder, so never stream it) ----------------
echo "[tri] adapter: snapshotting /tmp/wl.log over UART"
bash "$CCPA/scratchpad/uart_cmd.sh" "$OUT/adapter_wl.log" 14 'tail -c 65536 /tmp/wl.log' 2>&1
sleep 2
bash "$CCPA/scratchpad/uart_cmd.sh" "$OUT/adapter_supervisor.log" 14 'tail -60 /tmp/supervisor.log' 2>&1
sleep 2
bash "$CCPA/scratchpad/uart_cmd.sh" "$OUT/adapter_ocbmd.log" 12 'tail -30 /tmp/ocbmd.log' 2>&1

# ---- the interesting slices ------------------------------------------------------------------
grep -E "NETPROBE" "$OUT/headunit_full.log" > "$OUT/app_netprobe.log" 2>/dev/null
grep -iE "carplay|airplay|_carplay-ctrl|pair-verify|pair-setup|auth-setup|endpoint" \
    "$OUT/headunit_full.log" > "$OUT/headunit_carplay.log" 2>/dev/null
if [ -s "$OUT/iphone_syslog.log" ]; then
    grep -iE "carplay|airplay|carkit|accessoryd|pair-verify|pair-setup|auth-setup|endpoint|CRVehicle" \
        "$OUT/iphone_syslog.log" > "$OUT/iphone_carplay.log" 2>/dev/null
fi

echo "[tri] done. Files:"
for f in "$OUT"/*.log; do printf '  %8s  %s\n' "$(wc -l < "$f" | tr -d ' ')" "$(basename "$f")"; done
echo "[tri] start with iphone_carplay.log — iOS names the reason it rejected an accessory."
