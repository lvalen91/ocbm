#!/bin/sh
# phone_reset.sh — the AUTOMATED equivalent of the manual power cycle, for the PHONE-facing USB side
# ONLY (ci_hdrc.0). Task #24 / docs/carplay/02_SESSION_LIFECYCLE.md.
#
# It ports the proven OTG baseline reset from cold_start2.sh:35-41 — the piece that was missing from
# every automated re-arm path and the reason the docs/carplay/02_SESSION_LIFECYCLE.md wedge (iap2d flapping, ncm0 ifindex churn, OTG
# error latch) could only be cleared by a physical power cycle. It quiesces iap2d + the phone-facing
# gadget and returns the ci_hdrc.0 OTG controller to a known-clean A-host baseline; the next
# projection_up.sh then re-triggers the role switch from a clean state.
#
# HARD SCOPE: touches ONLY /sys/class/android_usb/android0 (phone gadget) and ci_hdrc.0/inputs (phone
# OTG). It MUST NEVER touch the Mac-facing OCBM link (android_usb_accessory / ci_hdrc.1) — doing so would
# drop the host connection. Do not add any ci_hdrc.1 path here.
#
# iap2d is killed FIRST so a surviving instance can't race its own host-gone path or hold /dev/i2c-1
# mid-sign (cold_start2.sh load-bearing note: the MFi chip must be free during a challenge-sign).
set -u
export PATH=/usr/sbin:/usr/bin:/sbin:/bin:$PATH
A=/sys/class/android_usb/android0
I=/sys/bus/platform/devices/ci_hdrc.0/inputs

echo "[reset] phone_reset: quiescing phone-facing side (ci_hdrc.0 only)"
pkill -f iap2d 2>/dev/null; sleep 1; pkill -9 -f iap2d 2>/dev/null

ifconfig ncm0 down 2>/dev/null                  # clear the stale ifindex / NO-CARRIER before the gadget drops

# OTG baseline reset — verbatim from the proven cold_start2.sh:36-41 (return ci_hdrc.0 to A-host, clear
# the error latch) so the iPhone re-enumerates as a plain device and projection_up can re-trigger.
echo 0 > "$A/enable"            2>/dev/null      # drop the iPhone-facing gadget
echo 0 > "$I/a_suspend_req_inf" 2>/dev/null      # stop requesting peripheral role
echo 1 > "$I/a_clr_err"         2>/dev/null      # clear the OTG error latch
echo 0 > "$I/a_bus_drop"        2>/dev/null
echo 1 > "$I/a_bus_req"         2>/dev/null      # ci_hdrc.0 back to A-host so the iPhone is a device
sleep 2
echo "[reset] phone_reset: done (gadget state=$(cat "$A/state" 2>/dev/null))"
