#!/bin/bash
# Continuous, UNFILTERED three-side capture for intermittent CarPlay faults.
#
#   head unit : adb logcat -b all  (main+system+crash+radio+events+kernel — NOT just the default 3)
#   iPhone    : pymobiledevice3 syslog live  (os_log; idevicesyslog does NOT carry os_log)
#   adapter   : LIVE UART console stream, `tail -F` over wl.log + supervisor.log + ocbmd.log
#
# All three auto-restart: a USB glitch on a moving vehicle otherwise ends a leg silently and you
# find out an hour later with nothing recorded.
#
# >>> THE UART IS A SINGLE-HOLDER PORT. While this runs, uart_cmd.sh / uart_push.sh WILL FAIL with
# >>> "open failed (busy?)". Stop the capture before issuing any box command.
#
# tri_capture.sh deliberately refused to stream the UART; this streams it because an intermittent
# fault needs the box's side continuously, not sampled. The box-side tail is re-armed every 10 min
# (Ctrl-C then re-issue) so a box reboot cannot silently end the adapter leg.
#
# Usage:  bash tools/drive_capture.sh            (new directory)
#         OUT_OVERRIDE=<dir> bash tools/drive_capture.sh   (append to an existing capture)
# Repo-relative roots. This project lives at ccpa_custom/host/gm_ccpa, so the tools resolve both
# roots from their own location rather than hard-coding an absolute path — moving the checkout, or
# having a second one, must not silently build against the wrong tree.
GM_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")/.." && pwd)"
CCPA_ROOT="$(cd "$GM_ROOT/../.." && pwd)"

set -uo pipefail

PORT=/dev/cu.usbserial-0001
ROOT="$GM_ROOT"
OUT="${OUT_OVERRIDE:-$ROOT/evidence/drive_$(date -u +%Y%m%d-%H%M%S)}"
mkdir -p "$OUT"
echo "$OUT" > "$ROOT/.drive_capture_dir"
rm -f "$OUT"/.pid_* 2>/dev/null

{ echo "started_utc=$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
  echo "mac_epoch=$(date +%s)"
  echo "headunit=$(adb shell 'date' 2>/dev/null | tr -d '\r')"
  echo "headunit_epoch=$(adb shell 'date +%s' 2>/dev/null | tr -d '\r')"
  echo "offset_hu_minus_mac_s=$(( $(adb shell 'date +%s' 2>/dev/null | tr -d '\r') - $(date +%s) ))"
  echo "app_uid=$(adb shell "dumpsys package zeno.gmccpa | sed -n 's/.*userId=\([0-9]*\).*/\1/p' | head -1" 2>/dev/null | tr -d '\r')"
  echo "log_tag=$(adb shell 'getprop persist.log.tag' 2>/dev/null | tr -d '\r')"
} >> "$OUT/clock_and_env.txt" 2>&1

# ---- head unit: ALL buffers, unfiltered --------------------------------------------------------
( while :; do
    adb wait-for-device >/dev/null 2>&1
    adb logcat -b all -v threadtime >> "$OUT/headunit.log" 2>>"$OUT/headunit.stderr"
    echo "### logcat ended $(date -u '+%FT%TZ') — restarting" >> "$OUT/headunit.log"
    sleep 2
  done ) & echo $! > "$OUT/.pid_hu"

# ---- iPhone: unfiltered os_log -----------------------------------------------------------------
( while :; do
    if pymobiledevice3 usbmux list 2>/dev/null | grep -q Identifier; then
        pymobiledevice3 syslog live >> "$OUT/iphone.log" 2>>"$OUT/iphone.stderr"
        echo "### iphone ended $(date -u '+%FT%TZ') — restarting" >> "$OUT/iphone.log"
    fi
    sleep 5
  done ) & echo $! > "$OUT/.pid_ios"

# ---- adapter: live console stream (shared implementation, proof-of-life re-arm) ---------------
bash "$ROOT/tools/uart_leg.sh" "$OUT" 300 & echo $! > "$OUT/.pid_uart"

echo "[drive] capturing (unfiltered, continuous) into $OUT"
echo "[drive]   head unit : logcat -b all"
echo "[drive]   iPhone    : os_log"
echo "[drive]   adapter   : live UART console  <-- uart_cmd.sh is BLOCKED while this runs"
echo "[drive] mark:  bash tools/mark.sh 'what I just did'"
echo "[drive] stop:  bash tools/drive_stop.sh"
