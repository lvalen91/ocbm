#!/bin/sh
# run_ocbmd.sh — inittab ::respawn wrapper for ocbmd (task #28).
#
# Gives ocbmd (the OCBM control plane + presence signal) PID-1 respawn protection. COEXISTS with
# ocbm_boot.sh's boot-time launch: waits for the accessory gadget, watches the running ocbmd, and only
# relaunches it when gone — never a double launch. A relaunched ocbmd re-opens /dev/usb_accessory and
# re-inits /tmp/host_present=0, so the host app re-SUBSCRIBEs (same as the L2 restart path).
export PATH=/usr/sbin:/usr/bin:/sbin:/bin:$PATH

# --- TEMPORARY deploy dead-man (remove once the running ocbmd is verified) --------------------------
# ocbmd is the ONLY channel to this box: no NCM while the host holds the gadget, and no serial. A bad
# ocbmd is therefore unrecoverable from here, and the failure seen on 2026-08-15 was "runs but never
# answers HELLO", which a crash-counter would not catch. So: unless something proves the new binary
# works by creating /tmp/ocbm_ok, restore the known-good copy and reboot. /tmp is tmpfs, so this
# re-arms on every boot until the file below is deleted.
if [ ! -e /tmp/ocbm_deadman ] && [ -e /usr/sbin/ocbmd.orig ] && [ -e /script/ocbm_deadman_on ]; then
  touch /tmp/ocbm_deadman
  ( sleep 240
    [ -e /tmp/ocbm_ok ] && exit 0
    echo "[deadman] no /tmp/ocbm_ok after 240s — restoring ocbmd.orig and rebooting" >> /script/ocbm_failover.log
    cp /usr/sbin/ocbmd.orig /usr/sbin/ocbmd
    sync
    reboot
  ) >/dev/null 2>&1 &
fi
# ---------------------------------------------------------------------------------------------------

sleep 2
while [ ! -e /dev/usb_accessory ]; do sleep 1; done
while pgrep -f /usr/sbin/ocbmd >/dev/null 2>&1; do sleep 3; done
echo "[respawn] ocbmd down -> relaunching (init)" >> /tmp/box.log
exec /usr/sbin/ocbmd >> /tmp/box.log 2>&1
