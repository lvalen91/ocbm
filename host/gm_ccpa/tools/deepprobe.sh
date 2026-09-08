#!/bin/bash
# Read-only deep ADB recon of the gminfo37 radio. No state changes, no i2c bus I/O.
GM_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")/.." && pwd)"
mkdir -p "$GM_ROOT/logs"

ADB=~/Library/Android/sdk/platform-tools/adb
OUT="$GM_ROOT/logs"/radio_deepprobe_$(date +%Y%m%d_%H%M%S).txt
sh() { echo; echo "----- $1 -----"; shift; "$ADB" shell "$@" 2>&1; }

{
echo "==== DEEP RADIO PROBE $(date) ===="

echo; echo "##### A. :7000 OWNER + NATIVE RECEIVER #####"
sh "listeners (ss)" "ss -tulpn 2>/dev/null | grep -E ':7000|:5000|:7001|:6363|:49156|:3689' "
sh "netstat 7000" "netstat -tlnp 2>/dev/null | grep -E ':7000|:7001|:5000'"
sh "processes: cinemo/carplay/airplay/rtsp" "ps -A 2>/dev/null | grep -iE 'cinemo|carplay|airplay|rtsp|raop' | grep -v grep"
sh "all native /vendor & system daemons (non-app)" "ps -A -o PID,USER,NAME 2>/dev/null | grep -vE 'u0_a|com\.|android\.|:|^ *PID' | head -60"
sh "vendor bin names" "ls /vendor/bin 2>/dev/null | grep -iE 'cinemo|carplay|airplay|rtsp|projection|mfi|apple'; ls /vendor/bin/hw 2>/dev/null | grep -iE 'cinemo|carplay|airplay|projection'; ls /system/bin 2>/dev/null | grep -iE 'cinemo|carplay|airplay'"
sh "init rc mentions" "cat /vendor/etc/init/*.rc /system/etc/init/*.rc /odm/etc/init/*.rc 2>/dev/null | grep -iE 'service .*(cinemo|carplay|airplay|projection|rtsp)|class_start|apple' | head -30"
sh "cinemo files" "ls -la /vendor/lib*/*inemo* /vendor/bin/*inemo* /system/lib*/*inemo* 2>/dev/null | head"

echo; echo "##### B. WIFI / SOFTAP / CLIENTS / BAND #####"
sh "softap config + info" "dumpsys wifi 2>/dev/null | grep -iE 'SoftAp|mBand|mChannel|apInterface|CountryCode|connectedClient|NumClients|mApConfig|Frequency|standard' | head -40"
sh "iw available?" "command -v iw; ls /vendor/bin/iw /system/bin/iw 2>/dev/null"
sh "iw dev + info (if present)" "iw dev 2>/dev/null; for i in wlan0 wlan1 wlan2; do echo == \$i ==; iw dev \$i info 2>/dev/null; iw dev \$i station dump 2>/dev/null | grep -E 'Station|signal|rx bit|tx bit' | head; done"
sh "ip neigh (ARP: who's on br0)" "ip neigh 2>/dev/null | grep -iE 'br0|192.168.5|172.20.10'; echo '--- all neigh ---'; ip neigh 2>/dev/null | head -30"
sh "br0 + wlan addrs" "ip -o addr 2>/dev/null | grep -E 'br0|wlan|172.20'"
sh "wifi client (station) state" "dumpsys wifi 2>/dev/null | grep -iE 'mWifiInfo|SSID:|Supplicant state|iPhone' | head -10"

echo; echo "##### C. iPhone / mDNS from shell #####"
sh "arp table raw" "cat /proc/net/arp 2>/dev/null"
sh "any 172.20.10 or 192.168.5 sockets" "netstat -tn 2>/dev/null | grep -E '172.20.10|192.168.5' | head"

echo; echo "##### D. USB / CCPA ADAPTER #####"
sh "usb state props" "getprop | grep -iE 'sys.usb|usb.config|usbhost'"
sh "dumpsys usb" "dumpsys usb 2>/dev/null | head -60"
sh "CCPA VID 1314 / any usb serial" "getprop | grep -iE '1314|carlink|ccpa'; ls /dev/bus/usb/*/* 2>/dev/null | head"
sh "input devices (HID from CCPA?)" "cat /proc/bus/input/devices 2>/dev/null | grep -iE 'Name|Vendor|Product' | head -30"

echo; echo "##### E. BLUETOOTH (CCPA BT path) #####"
sh "bt enabled + bonded devices" "settings get global bluetooth_on; dumpsys bluetooth_manager 2>/dev/null | grep -iE 'enabled|state|Bonded|name =|address|Carlinkit|CCPA' | head -30"

echo; echo "##### F. AUDIO ROUTING (receiver audio) #####"
sh "audio devices/output" "dumpsys audio 2>/dev/null | grep -iE 'Devices:|out_devices|Stream|- STREAM_|BUS|usage' | head -40"
sh "car audio zones" "dumpsys car_service 2>/dev/null | grep -iE 'CarAudioZone|BUS|context|address=bus' | head -30"

echo; echo "##### G. PACKAGES: full + enabled state of key ones #####"
sh "carplay/connection/tether/vending enabled state" "for p in com.gm.hmi.applecarplay com.gm.hmi.connection com.gm.hmi.androidauto com.android.vending com.android.networkstack.tethering.inprocess com.google.android.gms; do echo -n \"\$p: \"; dumpsys package \$p 2>/dev/null | grep -m1 -iE 'enabled=|enabledSetting' || echo MISSING; done"
sh "total package count" "pm list packages 2>/dev/null | wc -l; echo disabled:; pm list packages -d 2>/dev/null | wc -l"
sh "any apple/airplay/projection packages" "pm list packages 2>/dev/null | grep -iE 'apple|airplay|projection|cinemo|carlink'"

echo; echo "##### H. DISTRACTION / DISPLAY / OCCUPANT #####"
sh "driving state + speed" "dumpsys car_service 2>/dev/null | grep -iE 'DrivingState|Current Driving|SafetyRegion|PERF_VEHICLE_SPEED|speed' | head"
sh "displays summary" "dumpsys display 2>/dev/null | grep -iE 'Display Id|uniqueId|type=|2400|flags=|Cluster' | head -20"
sh "occupant zones" "dumpsys car_service 2>/dev/null | grep -iE 'zoneId=|displayType=|topActivity' | head"

echo; echo "##### I. STORAGE (for log pull) #####"
sh "df + shared storage" "df 2>/dev/null | grep -iE 'emulated|storage|data'; echo '--- /storage ---'; ls -la /storage 2>/dev/null; ls -la /sdcard/Download 2>/dev/null | head"
sh "usb mounts (vfat)" "mount 2>/dev/null | grep -iE 'vfat|usb|fuse' | head"

echo; echo "##### J. SELINUX / SHELL CAPABILITY #####"
sh "shell context + i2c label" "id; ls -Z /dev/i2c-0 /dev/i2c-1 2>/dev/null"
sh "can shell read hostapd.conf" "ls -la /data/vendor/wifi/hostapd/ 2>/dev/null; head -1 /data/vendor/wifi/hostapd/hostapd.conf 2>/dev/null || echo 'denied/absent'"

echo; echo "##### K. NETPROBE APP LOG (if user ran it) #####"
sh "NETPROBE logcat" "logcat -d -s NETPROBE 2>/dev/null | tail -120"

echo; echo "==== DONE  -> $OUT ===="
} 2>&1 | tee "$OUT"
echo "SAVED=$OUT"
