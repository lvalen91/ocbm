#!/bin/sh
# cold_start_now.sh — bring the ALREADY-CONNECTED iPhone into projection mode (no physical replug),
# reach Identified, bring up ncm0, then launch the presence-driven session supervisor (docs/carplay/02_SESSION_LIFECYCLE.md).
# The iPhone-facing gadget is on ci_hdrc.0; the Mac-facing OCBM accessory is ci_hdrc.1, so reconfiguring
# the iPhone gadget here does NOT disturb the OCBM link. Binaries are FHS-installed (iap_role_switch in
# /usr/bin; iap2d/airplayd/rx_connect in /usr/sbin); scripts in /script. Watch /tmp/iap2d.log for
# 'Identified'.
set -u
export PATH=/usr/sbin:/usr/bin:/sbin:/bin:$PATH
A=/sys/class/android_usb/android0
I=/sys/bus/platform/devices/ci_hdrc.0/inputs
L=/tmp/iap2d.log
: > "$L"
pkill -f iap2d 2>/dev/null; pkill -f airplayd 2>/dev/null; pkill -f rx_connect 2>/dev/null
pkill -f session_supervisor 2>/dev/null

DN=$(lsusb | grep 05ac | sed 's/.*Device \([0-9]*\):.*/\1/' | head -1)
[ -z "$DN" ] && { echo "[csn] no iPhone (05ac) on the bus"; exit 1; }
echo "[csn] iPhone at /dev/bus/usb/001/$DN — reconfiguring gadget + triggering role switch"

echo 0 > "$A/enable" 2>/dev/null
echo 239 > "$A/bDeviceClass"; echo 2 > "$A/bDeviceSubClass"; echo 1 > "$A/bDeviceProtocol"
echo "Magic Communication Tec." > "$A/iManufacturer"; echo "Auto Box" > "$A/iProduct"
echo 08e4 > "$A/idVendor"; echo 01c0 > "$A/idProduct"
echo 02:ca:fe:00:00:01 > "$A/f_ncm/ethaddr" 2>/dev/null
echo iap2,ncm > "$A/functions"
echo 1 > "$I/a_suspend_req_inf" 2>/dev/null
iap_role_switch "/dev/bus/usb/001/$DN" >> "$L" 2>&1   # /usr/bin/iap_role_switch
sleep 2
echo 1 > "$A/enable"
j=0; while [ "$j" -lt 6 ] && [ ! -e /dev/android_iap2 ]; do j=$((j + 1)); sleep 1; done
echo "[csn] gadget enabled; launching iap2d"
setsid iap2d /dev/android_iap2 >> "$L" 2>&1 &     # /usr/sbin/iap2d

i=0
while [ "$i" -lt 60 ]; do
  grep -q 'Identified' "$L" && break
  grep -q 'exit state' "$L" && break
  i=$((i + 1)); sleep 0.5
done
if ! grep -q 'Identified' "$L"; then
  echo "[csn] did NOT reach Identified: $(tail -1 "$L")"; exit 1
fi
echo "[csn] IDENTIFIED — bringing up ncm0, launching session supervisor"
echo 0 > /proc/sys/net/ipv6/conf/ncm0/disable_ipv6 2>/dev/null
ifconfig ncm0 up
sleep 1
setsid /script/session_supervisor.sh > /tmp/supervisor.log 2>&1 &
sleep 1
echo "[csn] supervisor=$(pgrep -f session_supervisor) iap2d=$(pgrep -f iap2d) — waiting for host SUBSCRIBE"
echo "[csn] next: on the Mac run 'ocbm-host session' to arm the session (present=1)"
