#!/bin/sh
########################
# wlan_on.sh - on-demand WiFi AP bring-up (NXP IW416). NCM stays up independently.
# SSID = ccpa-<4 hex from WiFi MAC> (per-device unique). Usage: wlan_on.sh [AP]
########################
export PATH=/usr/sbin:/usr/bin:/sbin:/bin:/tmp/bin:$PATH
KO=/lib/firmware/nxp/iw416_ko.tar.gz

# S11: take the SAME lock radio_hal.sh uses for this subsystem (/tmp/.radio_lock_wlan), mirroring
# its mkdir test-and-set + PID-liveness reclaim idiom (radio_hal.sh:84-99), so this path and
# radio_hal.sh wifi_ap_on never both invoke the vendor start_bluetooth_wifi.sh at once.
_wl_lk=/tmp/.radio_lock_wlan
while ! mkdir "$_wl_lk" 2>/dev/null; do
	_wl_holder=$(cat "$_wl_lk/pid" 2>/dev/null)
	if [ -n "$_wl_holder" ] && [ ! -d "/proc/$_wl_holder" ]; then
		rm -rf "$_wl_lk"; continue
	fi
	echo "[wlan_on] radio lock held by pid=${_wl_holder:-unknown} - refusing to stack AP bring-up" >> /tmp/box.log
	exit 1
done
echo $$ > "$_wl_lk/pid" 2>/dev/null
trap 'rm -rf "$_wl_lk" 2>/dev/null' EXIT
trap 'rm -rf "$_wl_lk" 2>/dev/null; trap - EXIT; exit 143' INT TERM HUP

# device-unique name: last 2 octets of WiFi MAC (fallback: last 4 hex of serial)
ID=$(set_wifi_mac 2>/dev/null | grep -oE '([0-9A-Fa-f]{2}:){5}[0-9A-Fa-f]{2}' | head -1)
SUF=$(echo "$ID" | sed 's/.*\(..\):\(..\)$/\1\2/' | tr 'A-F' 'a-f')
[ -n "$SUF" ] || SUF=$(cat /etc/serial_number 2>/dev/null | tr -cd '0-9a-fA-F' | tr 'A-F' 'a-f' | sed 's/.*\(....\)$/\1/')
NAME="ccpa-$SUF"

# 1) ensure NXP WiFi driver loaded
if ! grep -q "^moal " /proc/modules; then
	[ -e /tmp/mlan.ko ] || tar -xf "$KO" -C /tmp 2>/dev/null
	echo "[wlan_on] insmod mlan/moal"
	insmod /tmp/mlan.ko 2>/dev/null
	insmod /tmp/moal.ko mod_para=nxp/wifi_mod_para.conf 2>/dev/null
fi
# 2) wait for wlan0
i=0; while [ ! -e /sys/class/net/wlan0 ] && [ "$i" -lt 100 ]; do i=$((i+1)); sleep 0.1; done
[ -e /sys/class/net/wlan0 ] || { echo "[wlan_on] ERROR: wlan0 never appeared"; exit 1; }
set_wifi_mac 2>/dev/null
# 3) set AP SSID = ccpa-#### (hostapd.conf is the broadcast source for AP mode)
sed -i "s/^ssid=.*/ssid=$NAME/" /etc/hostapd.conf
echo "$NAME" > /etc/wifi_name
# 4) raise AP (ONDEMAND_WIFI overrides the ncm_only suppression guard)
ONDEMAND_WIFI=1 /script/start_bluetooth_wifi.sh AP
echo "[wlan_on] SSID=$NAME $(ifconfig wlan0 2>/dev/null | grep -o 'inet addr:[0-9.]*')  hostapd=$(ps|grep -v grep|grep -c '[ /]hostapd')"
