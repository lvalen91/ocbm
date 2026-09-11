#!/bin/bash
# adb_radio_probe.sh — read-only reconnaissance of an AAOS head unit over ADB.
#
# Characterizes the platform for the direct-WiFi CarPlay-receiver question: build/SELinux posture,
# what CarPlay/projection/Play components exist, driver-distraction config, network + WiFi + hotspot
# state, USB/I2C access (the CCPA + MFi path), and what a sideloaded app is actually granted.
#
# It CHANGES NOTHING. Every command is a getprop / dumpsys / cmd ... list|get / cat of a world-readable
# node. Output is teed to a timestamped log next to this script.
#
# Usage:
#   ./adb_radio_probe.sh                 # probe the only/attached device
#   ANDROID_SERIAL=<serial> ./adb_radio_probe.sh
#   APP=zeno.gmccpa ./adb_radio_probe.sh    # also introspect a sideloaded app's grants
#   USERS="0 10" ./adb_radio_probe.sh       # which Android users to enumerate packages for
#
# Packages are PER ANDROID USER. `pm list packages` with no --user reports the CALLING user, which
# for adb shell is user 0 — and this app installs `--user 10` (the driver's user on gminfo37, see
# docs/06 §86). The 2026-09-09 bundle was read with that default and made several user-10 packages,
# this app included, look uninstalled when all it established was "not on user 0". Every package
# query below therefore names its user in the output, so the distinction survives into the capture.

set -u
ADB="${ADB:-adb}"
APP="${APP:-zeno.gmccpa}"
USERS="${USERS:-0 10}"
STAMP="$(date +%Y%m%d_%H%M%S)"
OUT="$(cd "$(dirname "$0")" && pwd)/radio_probe_${STAMP}.txt"

# Run an adb shell command, labeled, tee'd. Never aborts the script on failure.
sh() { echo; echo "----- $1 -----"; shift; $ADB shell "$@" 2>&1; }
have() { $ADB shell "command -v $1 >/dev/null 2>&1 && echo yes || echo no" 2>/dev/null; }
# Same as sh(), but runs the command once per Android user with `--user <u>` appended and a
# `[user <u>]` label on every line. `$1` label, `$2` the pm command (without --user), `$3` optional
# filter pipeline. Empty output for a user prints "(none on user <u>)" — an explicit absence, not
# a blank that could be mistaken for a probe that did not run.
sh_users() {
  echo; echo "----- $1 (per user: $USERS) -----"
  local cmd="$2" filt="${3:-cat}"
  for u in $USERS; do
    # One device-side invocation per user; the output is captured there so "empty" is decided on
    # the same run that is printed (a second run could differ).
    $ADB shell "out=\$($cmd --user $u 2>&1 | $filt); if [ -n \"\$out\" ]; then printf '%s
' \"\$out\" | sed 's/^/[user $u] /'; else echo '[user $u] (none on user $u)'; fi" 2>&1
  done
}

{
echo "=================================================================="
echo " AAOS RADIO PROBE   $STAMP"
echo "=================================================================="

echo; echo "##### 0. DEVICE #####"
$ADB devices -l
$ADB get-state 2>&1
sh "whoami / id (adb shell uid)" id

echo; echo "##### 1. BUILD & SECURITY POSTURE #####"
sh "build props" "getprop | grep -E 'ro\.(build\.(type|tags|version\.(sdk|release)|fingerprint|flavor)|product\.(model|name|device|manufacturer)|debuggable|secure|adb\.secure|boot\.verifiedbootstate|oem_unlock)'"
sh "SELinux enforce state" getenforce
sh "GM / gmcable / harman props" "getprop | grep -iE 'gm\.|harman|gmcable|ota|dcm|projection' | head -40"
sh "verified boot / AVB" "getprop | grep -iE 'verifiedboot|avb|veritymode'"

echo; echo "##### 2. CARPLAY / PROJECTION / PLAY COMPONENTS #####"
sh "android users (packages are per user; adb shell defaults to user 0)" "pm list users"
sh_users "packages matching carplay/projection/auto/gearhead" "pm list packages -f" "grep -iE 'carplay|projection|gearhead|android.car|auto|mirror'"
sh_users "Play / GMS present" "pm list packages" "grep -iE 'com.android.vending|com.google.android.gms|gsf'"
sh_users "GM apps" "pm list packages" "grep -iE 'com.gm'"
sh "who holds the projection permissions" "dumpsys package | grep -iE 'gm.permission.(READ|WRITE)_PROJECTION_INFO' | head"
sh "GMCarPlay package detail (uid / sharedUser / installer)" "dumpsys package com.gm.hmi.applecarplay | grep -iE 'userId=|sharedUser|installerPackageName|versionName|codePath' | head"

echo; echo "##### 3. DRIVER DISTRACTION (CarUxRestrictions) #####"
sh "car_service help (does shell reach it?)" "cmd car_service --help 2>&1 | head -40"
sh "UX restriction / driving state dump" "dumpsys car_service 2>&1 | grep -iE 'uxr|restriction|driving|distraction|requiresDistractionOptimization' | head -40"
sh "distraction-optimized allowlist (activity blocking)" "dumpsys activity 2>&1 | grep -iE 'distraction|blocking|ActivityBlockingActivity' | head -20"

echo; echo "##### 4. NETWORK / WIFI / HOTSPOT #####"
sh "ip addr" "ip -o addr 2>/dev/null || ifconfig -a"
sh "ip route" "ip route 2>/dev/null; echo '--- rules ---'; ip rule 2>/dev/null"
sh "wifi state" "dumpsys wifi 2>&1 | grep -iE 'Wi-Fi is|mNetworkInfo|SSID|frequency|mWifiInfo|Supplicant state' | head -20"
sh "soft AP / tethering / hotspot" "dumpsys wifi 2>&1 | grep -iE 'softap|tether|apInterface|SoftApManager' | head; dumpsys connectivity 2>&1 | grep -iE 'tether' | head"
sh "SoftAP BAND / channel / freq (the AP the iPhone joins)" "dumpsys wifi 2>&1 | grep -iE 'mFrequency|mBand|channel|BandType|ApChannel|CenterFreq|mApConfig|country' | head -20; echo '--- hostapd ---'; cat /data/vendor/wifi/hostapd/hostapd.conf 2>/dev/null | grep -iE 'channel|hw_mode|freq|country|ssid' | head; ps -A 2>/dev/null | grep -i hostapd | grep -v grep"
sh "br0 / AP bridge addresses (iPhone's subnet)" "ip -o addr show br0 2>/dev/null; ip -o addr show dev wlan1 2>/dev/null; ip neigh 2>/dev/null | grep -iE '192.168.5|br0' | head"
sh "active networks (transports / validated / default)" "dumpsys connectivity 2>&1 | grep -iE 'NetworkAgentInfo|Active default|VALIDATED|WIFI|CELLULAR|ETHERNET' | head -30"
sh "netstat listeners" "netstat -tulnp 2>/dev/null | head -40 || ss -tulnp 2>/dev/null | head -40"
sh "multicast-capable ifaces" "ip -o link 2>/dev/null | grep -i multicast | head"

echo; echo "##### 5. USB / I2C (CCPA + MFi coprocessor path) #####"
sh "USB host / gadget state" "getprop | grep -iE 'sys.usb|usb.config|persist.sys.usb'"
sh "USB devices in sysfs" "ls -l /sys/bus/usb/devices/ 2>&1 | head; echo '--- lsusb ---'; lsusb 2>/dev/null | head"
sh "USB device VIDs/PIDs" "for d in /sys/bus/usb/devices/*/idVendor; do [ -f \"\$d\" ] && echo \"\$(cat \$d):\$(cat \${d%idVendor}idProduct) \$(cat \${d%idVendor}product 2>/dev/null)\"; done 2>/dev/null | head -30"
sh "i2c device nodes + perms (world access?)" "ls -l /dev/i2c-* 2>&1"
sh "i2c tools present" "command -v i2cdetect i2cget 2>&1"
sh "usb accessory / host permission service" "dumpsys usb 2>&1 | grep -iE 'host|accessory|connected|Manager' | head -20"

echo; echo "##### 6. DISPLAYS (cluster / multi-display) #####"
sh "displays" "dumpsys display 2>&1 | grep -iE 'Display Id|mDisplayId|uniqueId|type=|flags=|Cluster|Instrument' | head -40"
sh "occupant zones / car displays" "dumpsys car_service 2>&1 | grep -iE 'occupant|displayId|DisplayType|CLUSTER|zone' | head -30"

echo; echo "##### 7. INSTALL CAPABILITY / POLICY #####"
sh "unknown sources / install restrictions" "settings get global install_non_market_apps 2>&1; settings get secure install_non_market_apps 2>&1; dumpsys device_policy 2>&1 | grep -iE 'no_install|unknown_sources|no_debugging' | head"
sh "user restrictions" "dumpsys user 2>&1 | grep -iE 'restriction|no_install|no_debug' | head -20"
sh "verify adb installs" "settings get global verifier_verify_adb_installs 2>&1; settings get global package_verifier_enable 2>&1"

echo; echo "##### 8. SIDELOADED APP INTROSPECTION ($APP) #####"
sh_users "$APP installed?" "pm list packages" "grep -F $APP"
sh "$APP per-user install/enabled state (dumpsys 'User N:' lines)" "dumpsys package $APP 2>&1 | grep -E '^ *User [0-9]+:' | sed 's/^ *//'"
sh "$APP install source + uid" "dumpsys package $APP 2>&1 | grep -iE 'userId=|installerPackageName=|installInitiator|firstInstallTime|versionName|codePath|primaryCpuAbi' | head"
sh "$APP granted permissions" "dumpsys package $APP 2>&1 | grep -iE 'granted=true' | head -40"
sh "$APP appops (overlay etc.)" "cmd appops get $APP 2>&1 | head -30"
sh "$APP can draw overlays" "cmd appops get $APP SYSTEM_ALERT_WINDOW 2>&1"

echo; echo "##### 9. LOGCAT TAIL (NETPROBE + car UXR) #####"
sh "recent NETPROBE lines" "logcat -d -s NETPROBE 2>&1 | tail -40"
sh "recent distraction/blocking lines" "logcat -d 2>&1 | grep -iE 'CarUxRestrictions|ActivityBlocking|distraction' | tail -20"

echo
echo "=================================================================="
echo " DONE.  Full log: $OUT"
echo "=================================================================="
} 2>&1 | tee "$OUT"
