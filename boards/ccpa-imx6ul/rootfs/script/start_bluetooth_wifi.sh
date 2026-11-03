#!/bin/sh
# Slim IW416 hostapd AP bring-up — owned rewrite of start_bluetooth_wifi.sh.
# AP-only (no P2P/STA/multi-mode, no close_bluetooth_wifi). hostapd.conf is the
# single source of truth (ssid is set by wlan_on.sh). Called with ONDEMAND_WIFI=1.
export PATH=/usr/sbin:/usr/bin:/sbin:/bin:/tmp/bin:$PATH

if [ -e /script/ncm_only ] && [ -z "$ONDEMAND_WIFI" ]; then
	echo "[start_bluetooth_wifi] ncm_only -> AP suppressed (use wlan_on.sh)"; exit 0
fi
LOCK=/tmp/run_bluetooth_wifi_lock
[ -f "$LOCK" ] && { echo "[start_bluetooth_wifi] locked, exit"; exit 0; }
touch "$LOCK"

# channel + band from hostapd.conf (/etc/wifi_use_24G forces 2.4G ch6)
ch=$(sed -n 's/^channel=//p' /etc/hostapd.conf)
[ -e /etc/wifi_use_24G ] && ch=6
case "$ch" in ''|*[!0-9]*) ch=36 ;; esac
if [ "$ch" -ge 1 ] && [ "$ch" -le 14 ]; then
	grep -q "^hw_mode=a" /etc/hostapd.conf && sed -i "s/^hw_mode=a/hw_mode=g/" /etc/hostapd.conf
else
	grep -q "^hw_mode=g" /etc/hostapd.conf && sed -i "s/^hw_mode=g/hw_mode=a/" /etc/hostapd.conf
fi
sed -i "s/^channel=.*/channel=${ch}/" /etc/hostapd.conf

# AP IP — must NOT be 192.168.50.x (that is NCM's subnet). Point the wlan0 DHCP pool at it.
WLANIP=192.168.43.1
[ -e /etc/wifi_ip ] && WLANIP=$(cat /etc/wifi_ip)
base=${WLANIP%.*}
sed -i "s/192\.168\.[0-9]*\.100/${base}.100/; s/192\.168\.[0-9]*\.200/${base}.200/" /etc/udhcpd.conf 2>/dev/null
rm -f /var/lib/udhcpd.leases

# The AP interface NAME is not a constant across the CCPA variants: it is an insmod parameter
# (Realtek if2name=sta0, Broadcom iface_name=sta), and on Broadcom wlan0 does not exist until it
# is explicitly created on top of sta0. radio_hal.sh discovers the real name by enumerating
# sysfs and passes it in. The wlan0 default keeps the IW416 baseline byte-identical when the
# variable is absent, so this file stays correct with or without the seam.
IF=${RADIO_WLAN_IF:-wlan0}
echo 1 > /proc/sys/net/ipv6/conf/$IF/disable_ipv6 2>/dev/null
ifconfig "$IF" "$WLANIP" netmask 255.255.255.0 mtu 1500 up

# hostapd (AP)
test -e /tmp/bin/hostapd || cp /usr/sbin/hostapd /tmp/bin/hostapd 2>/dev/null
ps | grep -w hostapd | grep -v grep >/dev/null || hostapd /etc/hostapd.conf -B

# wlan0 DHCP server — explicit, separate from the NCM udhcpd (which serves ncm0 via its
# own /tmp config). The stock script masked this behind a global udhcpd check; fixed here.
ps | grep -v grep | grep -q "udhcpd .*etc/udhcpd.conf" || udhcpd /etc/udhcpd.conf

rm -f "$LOCK"
echo "[start_bluetooth_wifi] AP up: ssid=$(sed -n 's/^ssid=//p' /etc/hostapd.conf) ip=$WLANIP ch=$ch hostapd=$(ps|grep -v grep|grep -c '[ /]hostapd') udhcpd=$(ps|grep -v grep|grep -c '[ /]udhcpd')"
