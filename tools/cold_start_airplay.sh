#!/bin/sh
# cold_start_airplay.sh — cold start to Identified, bring up ncm0, then run the AirPlay pairing stack:
#   airplayd  (RTSP/pair-setup/pair-verify on :5000, local MFi chip)  +  rx_connect (mDNS _airplay._tcp)
# so the iPhone discovers the receiver, opens the AirPlay session, and we complete PAIR-VERIFY.
# Watch /tmp/airplayd.log for "PAIR-VERIFY COMPLETE". Needs /tmp/{iap2d,iap_role_switch,airplayd,rx_connect}.
set -u
A=/sys/class/android_usb/android0
I=/sys/bus/platform/devices/ci_hdrc.0/inputs
L=/tmp/iap2d.log; AL=/tmp/airplayd.log; RL=/tmp/rx_connect.log
pkill -f airplayd 2>/dev/null; pkill -f rx_connect 2>/dev/null; pkill -f iap2d 2>/dev/null
: > "$AL"; : > "$RL"
echo "[csa] start; waiting for iPhone replug (90s)" > "$L"

if lsusb | grep -q 05ac; then
  echo "[csa] iPhone present - waiting for UNPLUG first" >> "$L"
  while lsusb | grep -q 05ac; do sleep 1; done
fi
N=0; DN=
while [ "$N" -lt 90 ]; do
  DN=$(lsusb | grep 05ac | sed 's/.*Device \([0-9]*\):.*/\1/' | head -1)
  [ -n "$DN" ] && break
  N=$((N + 1)); sleep 1
done
[ -z "$DN" ] && { echo "[csa] TIMEOUT: no iPhone" >> "$L"; exit 1; }
echo "[csa] iPhone device = /dev/bus/usb/001/$DN" >> "$L"

echo 0 > "$A/enable" 2>/dev/null
echo 239 > "$A/bDeviceClass"; echo 2 > "$A/bDeviceSubClass"; echo 1 > "$A/bDeviceProtocol"
echo "Magic Communication Tec." > "$A/iManufacturer"; echo "Auto Box" > "$A/iProduct"
echo 08e4 > "$A/idVendor"; echo 01c0 > "$A/idProduct"
echo 02:ca:fe:00:00:01 > "$A/f_ncm/ethaddr" 2>/dev/null
echo iap2,ncm > "$A/functions"
echo 1 > "$I/a_suspend_req_inf"
/tmp/iap_role_switch "/dev/bus/usb/001/$DN" >> "$L" 2>&1
sleep 2
echo 1 > "$A/enable"
j=0; while [ "$j" -lt 5 ] && [ ! -e /dev/android_iap2 ]; do j=$((j + 1)); sleep 1; done
echo "[csa] gadget enabled; launching iap2d" >> "$L"
setsid /tmp/iap2d /dev/android_iap2 >> "$L" 2>&1 &

# wait for Identified (iap2d holds the iAP2 link once identified)
i=0
while [ "$i" -lt 60 ]; do
  grep -q 'Identified' "$L" && break
  grep -q 'exit state' "$L" && break
  i=$((i + 1)); sleep 0.5
done
if ! grep -q 'Identified' "$L"; then
  echo "[csa] did NOT reach Identified: $(tail -1 "$L")" >> "$AL"
  exit 1
fi
echo "[csa] IDENTIFIED — bringing up ncm0, starting airplayd + rx_connect" >> "$AL"

echo 0 > /proc/sys/net/ipv6/conf/ncm0/disable_ipv6 2>/dev/null
ifconfig ncm0 up
sleep 1
setsid /tmp/airplayd   >> "$AL" 2>&1 &   # RTSP/pairing on :5000 (all ifaces incl ncm0), local MFi
setsid /tmp/rx_connect >> "$RL" 2>&1 &   # mDNS advertise _airplay._tcp + browse _carplay-ctrl + connect-out
sleep 1
echo "[csa] airplayd=$(pgrep -f airplayd) rx_connect=$(pgrep -f rx_connect) iap2d_holding=$(ps|grep -c '[i]ap2d')" >> "$AL"
echo "[csa] ncm0 $(ifconfig ncm0 2>/dev/null | grep -o 'inet6 addr: fe80[^ ]*')" >> "$AL"
echo "[csa] armed — waiting for the iPhone to open AirPlay. tail -f /tmp/airplayd.log for PAIR-VERIFY." >> "$AL"
