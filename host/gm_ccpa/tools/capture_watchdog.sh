#!/bin/bash
# Additive watchdog for drive_capture.sh. Does NOT touch running streams — it only revives a leg
# whose supervising loop has itself died, and stops the capture before the disk fills.
#
# Each leg already self-restarts; this covers the case where the supervising subshell is killed
# (logout, pkill, OOM). Writes heartbeat.txt so liveness is checkable at a glance without pgrep.
# Repo-relative roots. This project lives at ccpa_custom/host/gm_ccpa, so the tools resolve both
# roots from their own location rather than hard-coding an absolute path — moving the checkout, or
# having a second one, must not silently build against the wrong tree.
GM_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")/.." && pwd)"
CCPA_ROOT="$(cd "$GM_ROOT/../.." && pwd)"

set -uo pipefail
ROOT="$GM_ROOT"
DIR="$(cat "$ROOT/.drive_capture_dir")"
MIN_FREE_GB="${MIN_FREE_GB:-25}"

while :; do
    NOW="$(date -u '+%FT%TZ')"
    FREE_GB=$(df -g / | tail -1 | awk '{print $4}')

    # Check the SUPERVISING LOOP by pid, not the streaming binary. While the cable is out the loop
    # is alive with no stream running — testing for the binary would spawn a duplicate loop, and on
    # reconnect two writers interleave into one file and silently corrupt it.
    HU_PID="$(cat "$DIR/.pid_hu" 2>/dev/null)"
    if [ -z "$HU_PID" ] || ! kill -0 "$HU_PID" 2>/dev/null; then
        echo "$NOW  radio leg dead — reviving" >> "$DIR/watchdog.log"
        ( while :; do
            adb wait-for-device >/dev/null 2>&1
            adb logcat -b all -v threadtime >> "$DIR/headunit.log" 2>>"$DIR/headunit.stderr"
            sleep 2
          done ) & echo $! > "$DIR/.pid_hu"
    fi

    IOS_PID="$(cat "$DIR/.pid_ios" 2>/dev/null)"
    if [ -z "$IOS_PID" ] || ! kill -0 "$IOS_PID" 2>/dev/null; then
        echo "$NOW  iphone leg dead — reviving" >> "$DIR/watchdog.log"
        ( while :; do
            pymobiledevice3 usbmux list 2>/dev/null | grep -q Identifier && \
              pymobiledevice3 syslog live >> "$DIR/iphone.log" 2>>"$DIR/iphone.stderr"
            sleep 5
          done ) & echo $! > "$DIR/.pid_ios"
    fi

    # Stop cleanly with the evidence intact rather than filling the volume.
    if [ "${FREE_GB:-999}" -lt "$MIN_FREE_GB" ]; then
        echo "$NOW  FREE=${FREE_GB}G < ${MIN_FREE_GB}G — STOPPING CAPTURE to protect the disk" >> "$DIR/watchdog.log"
        bash "$ROOT/tools/drive_stop.sh" >> "$DIR/watchdog.log" 2>&1
        exit 1
    fi

    { echo "last_check=$NOW"
      echo "free_gb=$FREE_GB"
      echo "radio_bytes=$(wc -c < "$DIR/headunit.log" 2>/dev/null)"
      echo "iphone_bytes=$(wc -c < "$DIR/iphone.log" 2>/dev/null)"
      echo "radio_loop=$(kill -0 "$(cat "$DIR/.pid_hu" 2>/dev/null)" 2>/dev/null && echo up || echo DOWN)"
      echo "iphone_loop=$(kill -0 "$(cat "$DIR/.pid_ios" 2>/dev/null)" 2>/dev/null && echo up || echo DOWN)"
      echo "radio_streaming=$(pgrep -f 'adb logcat -b all' >/dev/null && echo yes || echo no)"
      echo "iphone_streaming=$(pgrep -f 'pymobiledevice3 syslog live' >/dev/null && echo yes || echo no)"
    } > "$DIR/heartbeat.txt"

    sleep 60
done
