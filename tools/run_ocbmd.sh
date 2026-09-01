#!/bin/sh
# run_ocbmd.sh — inittab ::respawn wrapper for ocbmd (task #28).
#
# Gives ocbmd (the OCBM control plane + presence signal) PID-1 respawn protection. COEXISTS with
# ocbm_boot.sh's boot-time launch: waits for the accessory gadget, watches the running ocbmd, and only
# relaunches it when gone — never a double launch. A relaunched ocbmd re-opens /dev/usb_accessory and
# re-inits /tmp/host_present=0, so the host app re-SUBSCRIBEs (same as the L2 restart path).
export PATH=/usr/sbin:/usr/bin:/sbin:/bin:$PATH
sleep 2
while [ ! -e /dev/usb_accessory ]; do sleep 1; done
while pgrep -f /usr/sbin/ocbmd >/dev/null 2>&1; do sleep 3; done
echo "[respawn] ocbmd down -> relaunching (init)" >> /tmp/ocbmd.log
exec /usr/sbin/ocbmd >> /tmp/ocbmd.log 2>&1
