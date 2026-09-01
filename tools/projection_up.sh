#!/bin/sh
# projection_up.sh — bring the connected iPhone from normal mode into CarPlay PROJECTION: the iAP2
# handshake (→ Identified) + ncm0 up. This is the IDLE → projection-ready transition. It is NOT run at
# boot — the HOST APP commands it (session_supervisor.sh calls this on SUBSCRIBE), so an idle box with no
# host leaves the phone as a plain charging connection, never switched to projection. Idempotent: if
# iap2d already holds an Identified link it's a no-op success. Exit 0 = projection-ready, 1 = failed.
set -u
export PATH=/usr/sbin:/usr/bin:/sbin:/bin:$PATH
L=/tmp/iap2d.log
A=/sys/class/android_usb/android0
I=/sys/bus/platform/devices/ci_hdrc.0/inputs

# Already projection-ready? (holding pattern from a prior session — skip the handshake, just ncm0 up.)
# Liveness check via the iAP2 device node /dev/android_iap2 (present only while the iAP2 gadget is bound
# to a connected phone; disappears on unplug). NOTE: do NOT use `lsusb | grep 05ac` here — once the phone
# is in projection/accessory mode it no longer enumerates as Apple 05ac, so that check false-negatives a
# LIVE session. (The main handshake path below still uses 05ac, correct for a NORMAL-mode phone.)
if pgrep -f iap2d >/dev/null 2>&1 && grep -q Identified "$L" 2>/dev/null && [ -e /dev/android_iap2 ]; then
  echo "[proj] already Identified (iap2d holding a live link) — projection-ready"
  ifconfig ncm0 up 2>/dev/null
  exit 0
fi

# PREREQUISITE CHECK — exit 2 = "the BOX is missing something", distinct from exit 1 = "the phone
# would not cooperate". The supervisor must not climb the recovery ladder for this class: phone_reset,
# an ocbmd restart and a REBOOT all recover a wedged PHONE, and none of them can conjure a missing
# binary — the fault reproduces identically after every rung, so the ladder just burns the persistent
# reboot budget and drops the user's session on the way. Device-proven 2026-08-27: `iap_role_switch`
# was absent, so the 0x51 host-role switch never ran, the iPhone stayed a USB device, the phone-facing
# iAP2 gadget never bound and iap2d died at "detect write failed" — which the supervisor counted as
# three projection failures and answered with ESCALATE L1 twice, resetting a phone that was perfectly
# healthy. Naming it here also ends the bench misdiagnosis (it reads exactly like a locked phone or a
# bad cable) that cost the better part of a session.
for _t in iap_role_switch iap2d; do
  command -v "$_t" >/dev/null 2>&1 || {
    echo "[proj] BOX MISCONFIGURED: '$_t' is not installed (PATH=$PATH) — wired CarPlay cannot start."
    echo "[proj] This is NOT a phone fault; no reset or reboot fixes it. Install it (tools/ocbm_install.sh)."
    exit 2
  }
done

DN=$(lsusb | grep 05ac | sed 's/.*Device \([0-9]*\):.*/\1/' | head -1)
[ -z "$DN" ] && { echo "[proj] no iPhone (05ac) on the bus — cannot go projection-ready"; exit 1; }
echo "[proj] iPhone at /dev/bus/usb/001/$DN — iAP2 handshake → projection"
: > "$L"
pkill -f iap2d 2>/dev/null; sleep 1
echo 0 > "$A/enable" 2>/dev/null
echo 239 > "$A/bDeviceClass"; echo 2 > "$A/bDeviceSubClass"; echo 1 > "$A/bDeviceProtocol"
echo "Magic Communication Tec." > "$A/iManufacturer"; echo "Auto Box" > "$A/iProduct"
echo 08e4 > "$A/idVendor"; echo 01c0 > "$A/idProduct"
echo 02:ca:fe:00:00:01 > "$A/f_ncm/ethaddr" 2>/dev/null
echo iap2,ncm > "$A/functions"
echo 1 > "$I/a_suspend_req_inf" 2>/dev/null
iap_role_switch "/dev/bus/usb/001/$DN" >> "$L" 2>&1
sleep 2
echo 1 > "$A/enable"
j=0; while [ "$j" -lt 6 ] && [ ! -e /dev/android_iap2 ]; do j=$((j + 1)); sleep 1; done
setsid iap2d /dev/android_iap2 >> "$L" 2>&1 &
i=0
while [ "$i" -lt 60 ]; do
  grep -q Identified "$L" && break
  grep -q 'exit state' "$L" && break
  i=$((i + 1)); sleep 0.5
done
if ! grep -q Identified "$L"; then
  echo "[proj] did NOT reach Identified: $(tail -1 "$L" 2>/dev/null)"; exit 1
fi
echo "[proj] IDENTIFIED — projection-ready; bringing ncm0 up"
echo 0 > /proc/sys/net/ipv6/conf/ncm0/disable_ipv6 2>/dev/null
ifconfig ncm0 up
exit 0
