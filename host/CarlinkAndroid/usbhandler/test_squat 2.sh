#!/usr/bin/env bash
# GM AAOS "handler squat" proof — build, install to USER 10, and watch the silent grant. See docs/host/01_ANDROID_AND_AAOS.md.
#
# The user-10 target is load-bearing: a user-0 install resolves in the wrong UsbUserPermissionManager
# and does NOTHING (docs/host/01_ANDROID_AND_AAOS.md). adb reachability on a locked GM unit is its own problem (ADB is OTG-only
# on the center-console Type-C, CAN-gated) — that is the real risk this proof is meant to expose, not
# the framework behaviour. If `adb` cannot reach the unit, this script cannot run; that IS a finding.
set -euo pipefail

PKG="android.car.usb.handler"
USER_ID="${USER_ID:-10}"          # override only to A/B the user-0-does-nothing claim
TAG="UsbHandlerSquat"
HERE="$(cd "$(dirname "$0")/.." && pwd)"   # host/CarlinkAndroid
APK="$HOME/.cache/gradle-builds/carlink-android-ocbm/usbhandler/outputs/apk/debug/usbhandler-debug.apk"

echo "== build =="
"$HERE/gradlew" -p "$HERE" :usbhandler:assembleDebug --console=plain

echo "== adb device =="
adb get-state >/dev/null 2>&1 || { echo "!! no adb device — see header: on a locked GM unit this is EXPECTED and is itself the finding"; exit 2; }
adb shell pm list users | sed 's/^/   /'

echo "== install $PKG into user $USER_ID =="
# --user places the install in the foreground user; -g/-t not needed (no runtime perms declared).
adb install -r --user "$USER_ID" "$APK"
echo "   installed: $(adb shell pm list packages --user "$USER_ID" "$PKG")"

echo "== BEFORE-state: the dead-end line the framework logs today =="
echo "   (grep the current buffer; you want this to STOP appearing once we are present in user $USER_ID)"
adb logcat -d | grep -iE "Default USB handling package .*not found|deviceAttachedForFixedHandler|NameNotFound" | tail -5 || true

cat <<'HINT'

== NOW: plug the CCPA-OCBM (0x1314:0x2d00) into the head unit's host port ==
   Watching logcat for our probe. Expect, with NO permission dialog:
     UsbHostManagementActivity launched (fixed-handler path)
     [activity] attach 1314:2d00 (CCPA-OCBM) ...
     [activity] hasPermission=TRUE for 1314:2d00 — silent grant confirmed, no dialog
     [activity] CCPA-OCBM open OK: claimIf0=true bulkIn=0x83 bulkOut=0x02 fd=... — grant is real and usable
   And the framework's "Default USB handling package ... not found" line should NO LONGER fire.
   (Ctrl-C to stop.)

HINT
adb logcat -c || true
exec adb logcat -s "$TAG":V UsbHostManagementActivity:V UsbProfileGroupSettingsManager:V UsbDeviceManager:V
