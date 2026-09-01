#!/bin/sh
# cold_start3.sh — cold start to Identified, HOLD the link, bring up the phone-side ncm0, and
# observe whether the iPhone starts its post-identify AirPlay/mDNS discovery over IP.
#
# Extends cold_start2.sh: runs iap2d in the BACKGROUND (so it keeps holding the iAP2 link while
# we work), then on Identified brings ncm0 up (IPv6) and samples rx_packets to see the iPhone talk.
# Log: /tmp/iap2d.log (handshake) + /tmp/ncm.log (this script's ncm observations).
#
# Needs /tmp/iap2d + /tmp/iap_role_switch. Each run needs a fresh physical replug.
set -u
A=/sys/class/android_usb/android0
I=/sys/bus/platform/devices/ci_hdrc.0/inputs
L=/tmp/iap2d.log
NL=/tmp/ncm.log
: > "$NL"
echo "[cs3] start; waiting for iPhone replug (90s window)" > "$L"

if lsusb | grep -q 05ac; then
  echo "[cs3] iPhone present - waiting for UNPLUG first" >> "$L"
  while lsusb | grep -q 05ac; do sleep 1; done
fi
N=0; DN=
while [ "$N" -lt 90 ]; do
  DN=$(lsusb | grep 05ac | sed 's/.*Device \([0-9]*\):.*/\1/' | head -1)
  [ -n "$DN" ] && break
  N=$((N + 1)); sleep 1
done
[ -z "$DN" ] && { echo "[cs3] TIMEOUT: no iPhone in 90s" >> "$L"; exit 1; }
echo "[cs3] iPhone device = /dev/bus/usb/001/$DN" >> "$L"

echo 0 > "$A/enable" 2>/dev/null
echo 239 > "$A/bDeviceClass"; echo 2 > "$A/bDeviceSubClass"; echo 1 > "$A/bDeviceProtocol"
echo "Magic Communication Tec." > "$A/iManufacturer"; echo "Auto Box" > "$A/iProduct"
echo 08e4 > "$A/idVendor"; echo 01c0 > "$A/idProduct"
echo iap2,ncm > "$A/functions"
echo 1 > "$I/a_suspend_req_inf"
/tmp/iap_role_switch "/dev/bus/usb/001/$DN" >> "$L" 2>&1
sleep 2
echo 1 > "$A/enable"
echo "[cs3] gadget enabled; launching iap2d (background hold)" >> "$L"
setsid /tmp/iap2d /dev/android_iap2 >> "$L" 2>&1 &

# wait up to 30s for Identified (or an early exit)
i=0
while [ "$i" -lt 60 ]; do
  grep -q 'Identified' "$L" && break
  grep -q 'exit state' "$L" && break
  i=$((i + 1)); sleep 0.5
done

if ! grep -q 'Identified' "$L"; then
  echo "[cs3] did NOT reach Identified: $(tail -1 "$L")" >> "$NL"
  exit 1
fi

echo "[cs3] IDENTIFIED — bringing up phone-side ncm0 (IPv6)" >> "$NL"
echo 0 > /proc/sys/net/ipv6/conf/ncm0/disable_ipv6 2>/dev/null
ifconfig ncm0 up
sleep 2
{
  echo "=== ncm0 ==="; ifconfig ncm0
  echo "=== ncm0 IPv6 ==="; ip -6 addr show dev ncm0 2>/dev/null
} >> "$NL" 2>&1

# sample rx to see if the iPhone drives discovery (mDNS/ND/RS) toward us
R0=$(cat /sys/class/net/ncm0/statistics/rx_packets 2>/dev/null || echo 0)
T0=$(cat /sys/class/net/ncm0/statistics/tx_packets 2>/dev/null || echo 0)
sleep 15
R1=$(cat /sys/class/net/ncm0/statistics/rx_packets 2>/dev/null || echo 0)
echo "[cs3] ncm0 rx_packets ${R0}->${R1} (delta $((R1 - R0))) over 15s; tx_start=${T0}" >> "$NL"
echo "[cs3] neighbors:" >> "$NL"; ip -6 neigh show dev ncm0 2>/dev/null >> "$NL"
echo "[cs3] iap2d holding = $(ps | grep -c '[i]ap2d')" >> "$NL"
echo "[cs3] done — iap2d holds the link; inspect /tmp/ncm.log" >> "$NL"
