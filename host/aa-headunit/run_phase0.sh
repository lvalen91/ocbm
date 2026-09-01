#!/bin/sh
# Phase 0 runner: wait for the phone's AA head-unit server (TCP 5277), then run
# the head-unit client through an adb forward. See docs/host/02_ANDROID_AUTO.md.
set -e
BIN="$(dirname "$0")/target/release/aa-headunit"
LOG="${1:-/tmp/aa_phase0.log}"
adb forward tcp:5277 tcp:5277 >/dev/null
echo "[run] waiting for phone to listen on 5277 (start 'head unit server' in AA dev settings)..."
i=0
while [ "$i" -lt 600 ]; do
  # :149D = 5277 in hex; state 0A = LISTEN
  if adb shell "cat /proc/net/tcp /proc/net/tcp6 2>/dev/null" | awk '{print $2, $4}' | grep -qi ':149D 0A'; then
    echo "[run] phone is listening on 5277 — running client"
    "$BIN" 127.0.0.1:5277 2>&1 | tee "$LOG"
    adb forward --remove tcp:5277 >/dev/null 2>&1 || true
    exit 0
  fi
  i=$((i+1)); sleep 1
done
echo "[run] timed out after 600s waiting for the head-unit server"
adb forward --remove tcp:5277 >/dev/null 2>&1 || true
exit 1
