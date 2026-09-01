#!/bin/sh
# Waits for the phone's Android Auto head-unit server (TCP 5277), then runs the
# Phase-1 capture through an adb forward. See docs/host/02_ANDROID_AUTO.md.
#
# The head-unit server is started manually in the AA app (Developer settings ->
# Start head unit server) and stops when idle, so this polls until it appears.
set -u
D="$(dirname "$0")"
BIN="$D/target/release/aa-headunit"
LOG="${1:-/tmp/aa_phase1.log}"

[ -x "$BIN" ] || { echo "[run] $BIN not built (run: OPENSSL_DIR=\$(brew --prefix openssl@3) cargo build --release)"; exit 1; }

cleanup() { adb forward --remove tcp:5277 >/dev/null 2>&1; }
trap cleanup EXIT INT TERM

adb forward tcp:5277 tcp:5277 >/dev/null 2>&1 || { echo "[run] adb forward failed (device connected?)"; exit 1; }

echo "[run] waiting for head-unit server on 5277 (0x149D) ..."
i=0
while [ "$i" -lt 600 ]; do
  # /proc/net/tcp{,6} rows: "sl local:PORT remote:PORT ST ..."; field 2 = local,
  # field 4 = state. 0x149D = 5277, state 0A = LISTEN. Match local port + state.
  if adb shell "cat /proc/net/tcp /proc/net/tcp6 2>/dev/null" \
       | awk '{print $2, $4}' | grep -qi ':149D 0A'; then
    echo "[run] server up — capturing"
    "$BIN" 127.0.0.1:5277 /tmp/aa_capture.h264 120 > "$LOG" 2>&1
    rc=$?
    echo "[run] done (client exit $rc); log: $LOG"
    exit "$rc"
  fi
  i=$((i + 1))
  sleep 1
done
echo "[run] timed out after 600s waiting for the head-unit server"
exit 1
