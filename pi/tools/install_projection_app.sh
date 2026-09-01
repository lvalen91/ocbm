#!/usr/bin/env bash
# Install the AAOS CarPlay projection system app onto the Raspberry Pi.
#
# Two modes, and the difference matters:
#
#   --debug  (default)  adb install. Fast, no reboot, no remount. The app runs and every localhost
#                       seam works, but the platform DENIES the privileged permissions, so:
#                         * projection status is never published (AAOS does not know CarPlay exists)
#                         * steering-wheel voice/call keys do not reach CarPlay
#                         * the launcher tile falls back to FLAG_ACTIVITY_REORDER_TO_FRONT
#                       CarProjectionBridge is written to degrade exactly this way, so this is a
#                       perfectly usable configuration for working on video, audio and touch.
#
#   --system            Install to /system/priv-app with the privapp-permissions allowlist. Needs a
#                       writable /system and a reboot. This is what makes the AAOS integration real.
#
# Usage:
#   pi/tools/install_projection_app.sh [--debug|--system] [--serial <adb-serial>] [--release]

set -euo pipefail

MODE=debug
SERIAL=""
VARIANT=debug
PKG=com.carlink.projection

while [ $# -gt 0 ]; do
    case "$1" in
        --debug)   MODE=debug ;;
        --system)  MODE=system ;;
        --release) VARIANT=release ;;
        --serial)  SERIAL="$2"; shift ;;
        -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
    shift
done

here="$(cd "$(dirname "$0")/../.." && pwd)"
gradle_root="$here/host/CarlinkAndroid"
adb=(adb)
[ -n "$SERIAL" ] && adb=(adb -s "$SERIAL")

say() { printf '\033[1m==>\033[0m %s\n' "$*"; }

# ---- build -----------------------------------------------------------------------------------

# Capitalised without ${VAR^} — that is a bash 4 feature and macOS still ships bash 3.2, so the
# script would die on the developer machine it is most often run from.
case "$VARIANT" in
    debug)   GRADLE_TASK=":projection:assembleDebug" ;;
    release) GRADLE_TASK=":projection:assembleRelease" ;;
esac

say "building :projection ($VARIANT)"
( cd "$gradle_root" && ./gradlew "$GRADLE_TASK" -q )

# The build directory is redirected out of ~/Documents deliberately (iCloud sync writes conflict
# copies into a synced tree and Gradle then feeds them back into javac/D8), so resolve it from the
# same property the root build.gradle.kts uses rather than assuming ./build.
build_root="${CARLINK_BUILD_ROOT:-$HOME/.cache/gradle-builds/carlink-android-ocbm}"
apk=$(find "$build_root/projection/outputs/apk/$VARIANT" -name '*.apk' 2>/dev/null | head -1)
[ -n "$apk" ] || { echo "no APK found under $build_root/projection/outputs/apk/$VARIANT" >&2; exit 1; }
say "APK: $apk"

# ---- install ---------------------------------------------------------------------------------

if [ "$MODE" = debug ]; then
    say "installing as an ordinary app (privileged permissions will be DENIED)"
    "${adb[@]}" install -r -g "$apk"
    say "done. AAOS integration is degraded by design in this mode — see the header."
    say "check with: adb shell dumpsys car_service --services CarProjectionService"
    exit 0
fi

# ---- system install --------------------------------------------------------------------------

say "installing as a privileged system app"
"${adb[@]}" root >/dev/null 2>&1 || true
"${adb[@]}" wait-for-device
"${adb[@]}" remount

# The allowlist is what actually grants the signature|privileged permissions. Without it in
# /system/etc/permissions the platform refuses them even for an app in priv-app, and on some builds
# refuses to BOOT with a "privileged permission not in allowlist" fatal — which is why this file is
# pushed before the APK, never after.
allowlist="$here/pi/tools/privapp-permissions-com.carlink.projection.xml"
[ -f "$allowlist" ] || { echo "missing $allowlist" >&2; exit 1; }

say "pushing the priv-app permission allowlist"
"${adb[@]}" push "$allowlist" /system/etc/permissions/privapp-permissions-com.carlink.projection.xml
"${adb[@]}" shell chmod 644 /system/etc/permissions/privapp-permissions-com.carlink.projection.xml

say "removing any previously sideloaded copy"
# A /data copy shadows the /system one and would keep running WITHOUT the privileges.
"${adb[@]}" uninstall "$PKG" >/dev/null 2>&1 || true

say "pushing the APK to /system/priv-app"
"${adb[@]}" shell mkdir -p "/system/priv-app/CarlinkProjection"
"${adb[@]}" push "$apk" "/system/priv-app/CarlinkProjection/CarlinkProjection.apk"
"${adb[@]}" shell chmod 644 "/system/priv-app/CarlinkProjection/CarlinkProjection.apk"

say "rebooting (a priv-app is only picked up by a fresh package scan)"
"${adb[@]}" reboot

cat <<'EOF'

Rebooting. After boot, verify:

  # the app is privileged and holds the projection permission
  adb shell dumpsys package com.carlink.projection | grep -A3 "requested permissions"

  # AAOS can see the projection app
  adb shell dumpsys car_service --services CarProjectionService

  # the seams are listening (these must be bound BEFORE a session starts, or frames are dropped)
  adb shell 'ss -ltn | grep -E "9001|9002|9003"'

  # the generated config
  adb shell cat /tmp/carplay_cfg.yaml

EOF
