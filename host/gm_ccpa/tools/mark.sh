#!/bin/bash
# Label the current moment in ALL THREE logs at once, so a scenario can be found later.
# The head-unit marker goes through `log`, so it lands in the same ring as everything else.
# Repo-relative roots. This project lives at ccpa_custom/host/gm_ccpa, so the tools resolve both
# roots from their own location rather than hard-coding an absolute path — moving the checkout, or
# having a second one, must not silently build against the wrong tree.
GM_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")/.." && pwd)"
CCPA_ROOT="$(cd "$GM_ROOT/../.." && pwd)"

set -uo pipefail
ROOT="$GM_ROOT"
DIR="$(cat "$ROOT/.drive_capture_dir" 2>/dev/null)"
MSG="${*:-mark}"
TS="$(date -u '+%FT%TZ')"
adb shell "log -t NETPROBE '===== MARK: $MSG ====='" 2>/dev/null
[ -n "$DIR" ] && echo "$TS  $MSG" >> "$DIR/markers.log"
echo "[mark] $TS  $MSG"
