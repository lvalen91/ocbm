#!/bin/sh
# run_supervisor.sh — inittab ::respawn wrapper for session_supervisor.sh (task #28).
#
# Gives the supervisor PID-1 (busybox init) respawn protection: if it crashes or is OOM-killed, init
# restarts it. COEXISTS with ocbm_boot.sh's boot-time launch — this wrapper waits for the OCBM gadget,
# then watches the running supervisor and only (re)launches it when it is actually gone, so there is
# never a double launch. `exec` so init tracks the supervisor's PID directly. The `sleep`s keep init
# from hot-looping the respawn if the supervisor exits immediately.
export PATH=/usr/sbin:/usr/bin:/sbin:/bin:$PATH
sleep 3
# Wait for the OCBM accessory gadget (ocbm_boot.sh sets it up + launches the supervisor). This also
# orders us after start_main_service.sh, which rcS backgrounds.
while [ ! -e /dev/usb_accessory ]; do sleep 1; done
# Already running (ocbm_boot's launch, or a prior respawn)? Watch until it dies, then relaunch.
while pgrep -f /script/session_supervisor.sh >/dev/null 2>&1; do sleep 3; done
echo "[respawn] session_supervisor down -> relaunching (init)" >> /tmp/supervisor.log
exec /script/session_supervisor.sh >> /tmp/supervisor.log 2>&1
