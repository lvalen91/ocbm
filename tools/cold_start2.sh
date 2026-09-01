#!/bin/sh
# cold_start2.sh — phone-side wired-CarPlay cold start for ccpa_custom (runs ON the box).
#
# Brings the iPhone into CarPlay by role-switching it to USB host, presenting the iap2,ncm
# gadget, and running iap2d (the local-i2c iAP2 accessory L3 daemon) through auth + identify.
# Faithful to ncm_carplayd's proven iap2_carplay_test.sh / carplay_stack.sh, minus the
# NCM MFi signing service (ccpa_custom does MFi on the local chip inside iap2d).
#
# Deploy: push iap2d + iap_role_switch to the box (over OCBM `ocbm-host push`, or UART), then run
# this over the UART console. Each cold start needs a FRESH physical replug of the iPhone
# (the trigger + gadget-enable must fire while the iPhone is still enumerated as a device).
#
#   IAP2D=/tmp/iap2d TRIGGER=/tmp/iap_role_switch WAIT=90 ./cold_start2.sh
#
# *** LOAD-BEARING (grounded on-device, ncm_carplayd 2026-07-01) ***
# Nothing else may touch /dev/i2c-1 during iap2d's challenge-sign window. The chip NACKs reads
# for ~0.75-1.6 s while signing; a competing I2C transaction (e.g. an `ocbm-host mfi` request
# that makes ocbmd hit the chip) slows the sign past the iPhone's patience and it drops USB-host
# mode mid-sign -> auth dies at "host gone". So DO NOT issue any CH_MFI / MFi traffic from the
# host while this is running. ocbmd's one-shot boot warm-up read is fine (it is long done).

set -u
IAP2D="${IAP2D:-/tmp/iap2d}"
TRIGGER="${TRIGGER:-/tmp/iap_role_switch}"
WAIT="${WAIT:-90}"
LOG="${LOG:-/tmp/iap2d.log}"

A=/sys/class/android_usb/android0
I=/sys/bus/platform/devices/ci_hdrc.0/inputs

[ -x "$IAP2D" ]   || { echo "[cold] missing iap2d at $IAP2D (push it first)"; exit 1; }
[ -x "$TRIGGER" ] || { echo "[cold] missing iap_role_switch at $TRIGGER (push it first)"; exit 1; }

# --- teardown to a known-clean baseline (stale OTG/gadget state causes false results) ---
pkill -f iap2d 2>/dev/null
echo 0 > "$A/enable"            2>/dev/null   # drop the iPhone-facing gadget
echo 0 > "$I/a_suspend_req_inf" 2>/dev/null   # stop requesting peripheral role
echo 1 > "$I/a_clr_err"         2>/dev/null
echo 0 > "$I/a_bus_drop"        2>/dev/null
echo 1 > "$I/a_bus_req"         2>/dev/null   # ci_hdrc.0 back to A-host so the iPhone is a device
sleep 2

# --- wait for the iPhone to enumerate as a DEVICE (05ac:12a8 PTP mode) ---
echo "[cold] waiting up to ${WAIT}s for an iPhone replug (must appear as a USB device)..."
DN=""
i=0
while [ "$i" -lt "$WAIT" ]; do
    DN=$(lsusb | grep 05ac | sed 's/.*Device \([0-9]*\):.*/\1/' | head -1)
    [ -n "$DN" ] && break
    i=$((i + 1))
    sleep 1
done
[ -n "$DN" ] || { echo "[cold] no iPhone device after ${WAIT}s — REPLUG and rerun"; exit 1; }
DEVPATH="/dev/bus/usb/001/$DN"
echo "[cold] iPhone device = $DEVPATH"

# --- configure the phone-facing gadget (do NOT enable yet) ---
echo 0 > "$A/enable" 2>/dev/null
echo 239 > "$A/bDeviceClass"; echo 2 > "$A/bDeviceSubClass"; echo 1 > "$A/bDeviceProtocol"
echo "Magic Communication Tec." > "$A/iManufacturer"; echo "Auto Box" > "$A/iProduct"
echo 08e4 > "$A/idVendor"; echo 01c0 > "$A/idProduct"
echo 02:ca:fe:00:00:01 > "$A/f_ncm/ethaddr" 2>/dev/null   # distinct ncm1 host MAC
echo iap2,ncm > "$A/functions"

# --- role flip: prime peripheral, fire the Apple 0x51 trigger, then present the gadget ---
echo 1 > "$I/a_suspend_req_inf"
"$TRIGGER" "$DEVPATH"
sleep 2
echo 1 > "$A/enable"

# wait for the f_iap2 char device to appear before launching iap2d (it hard-exits if absent)
j=0
while [ "$j" -lt 5 ] && [ ! -e /dev/android_iap2 ]; do j=$((j + 1)); sleep 1; done
[ -e /dev/android_iap2 ] || echo "[cold] warn: /dev/android_iap2 not present yet (gadget state=$(cat "$A/state" 2>/dev/null))"
echo "[cold] gadget enabled; state=$(cat "$A/state" 2>/dev/null). starting iap2d -> $LOG"
echo "[cold] expect: SYN-ACK -> RX 0xAA00 -> TX 0xAA01 -> RX 0xAA02 -> TX 0xAA03 -> RX 0xAA05 -> RX 0x1D00 -> TX 0x1D01 -> RX 0x1D02 -> state=Identified"

# iap2d holds the link until the iPhone leaves; run it in the foreground so the console shows progress.
"$IAP2D" 2>&1 | tee "$LOG"
RC=$?

echo "[cold] iap2d exited rc=$RC (0=Authenticated+, 5=did not authenticate)"
echo 0 > "$A/enable"            2>/dev/null
echo 0 > "$I/a_suspend_req_inf" 2>/dev/null
echo "[cold] done"
