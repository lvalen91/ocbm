#!/bin/bash
# Repo-relative roots. This project lives at ccpa_custom/host/gm_ccpa, so the tools resolve both
# roots from their own location rather than hard-coding an absolute path — moving the checkout, or
# having a second one, must not silently build against the wrong tree.
GM_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")/.." && pwd)"
CCPA_ROOT="$(cd "$GM_ROOT/../.." && pwd)"

set -uo pipefail
ROOT="$GM_ROOT"
DIR="$(cat "$ROOT/.drive_capture_dir" 2>/dev/null)"
[ -z "$DIR" ] && { echo "no capture running"; exit 1; }
# Kill the watchdog FIRST — it revives dead legs every 60s and would undo this stop.
pkill -f capture_watchdog.sh 2>/dev/null
[ -f "$DIR/.pid_watchdog" ] && kill "$(cat "$DIR/.pid_watchdog")" 2>/dev/null
sleep 1
for p in "$DIR"/.pid_*; do [ -f "$p" ] && kill "$(cat "$p")" 2>/dev/null; done
sleep 1
pkill -f "adb logcat -b all" 2>/dev/null
pkill -f "pymobiledevice3 syslog live" 2>/dev/null
[ -f "$DIR/.pid_uart_cat" ] && kill "$(cat "$DIR/.pid_uart_cat")" 2>/dev/null
sleep 1
echo "[drive] stopped. UART released. Files in $DIR:"
for f in "$DIR"/*.log; do [ -f "$f" ] && printf '  %10s lines  %s\n' "$(wc -l < "$f" | tr -d ' ')" "$(basename "$f")"; done
