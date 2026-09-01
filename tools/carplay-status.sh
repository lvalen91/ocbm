#!/bin/sh
# carplay-status — one-glance lifecycle verdict for the CarPlay session (task #26 / docs/carplay/02_SESSION_LIFECYCLE.md).
# Reads the supervisor's canonical /tmp/carplay_state plus the raw signals, so a stuck/flapping session
# is a single command instead of the multi-log grep hunt that the docs/carplay/02_SESSION_LIFECYCLE.md diagnosis became.
set -u
export PATH=/usr/sbin:/usr/bin:/sbin:/bin:$PATH
S=/tmp/carplay_state

echo "=== CarPlay session state ==="
if [ -f "$S" ]; then
  cat "$S"
else
  echo "phase=? (no $S — is session_supervisor.sh running?)"
fi

echo "--- signals ---"
echo "host_present=$(cat /tmp/host_present 2>/dev/null)   session_healthy=$(cat /tmp/session_healthy 2>/dev/null)"
if [ -f /tmp/peer_pending ]; then echo "peer_pending=$(cat /tmp/peer_pending 2>/dev/null)"; fi
echo "iap2d=$(pgrep -f iap2d | tr '\n' ' ')  airplayd=$(pgrep -f airplayd | tr '\n' ' ')  ocbmd=$(pgrep -f ocbmd | tr '\n' ' ')"
echo "ncm0: $(ip -o link show ncm0 2>/dev/null | cut -c1-90 || echo 'n/a')"

echo "--- recent lifecycle transitions (uptime-stamped, #30) ---"
tail -8 /tmp/lifecycle.ndjson 2>/dev/null || echo "(none yet)"

echo "--- recent supervisor verdicts ---"
grep -E "health=STUCK|ESCALATE|ESTABLISHMENT STALL|milestone:|clearing STUCK|ARMED|TEARDOWN" /tmp/supervisor.log 2>/dev/null | tail -10
