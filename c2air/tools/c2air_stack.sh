#!/bin/sh
# Full C2Air wireless-CarPlay stack. Runs BEFORE the accessory switch (which takes ADB away).
exec >/tmp/stack.log 2>&1

# `pkill` has no symlink on this rootfs though busybox has the applet; av.rs shells out to a bare
# `pkill` to reap airplayd/rx-connect, so without this shim every reap silently ENOENTs.
mkdir -p /tmp/bin
[ -e /tmp/bin/pkill ] || ln -sf "$(command -v busybox || echo /bin/busybox)" /tmp/bin/pkill
export PATH=/tmp/bin:$PATH

# Kill every process whose cmdline contains $1, EXCEPT ourselves and our ancestors.
#
# `pkill -f <path>` is unusable here and has now bitten twice. It matches the FULL COMMAND LINE of
# every process, which includes the invoking shell — e.g. an `adb shell "chmod 755 ... /tmp/c2air-btattach"`
# wrapper, or this script's own launcher. pkill then kills the parent and this script dies with it,
# silently, part-way through bring-up. And `pkill -x <name>` is equally unusable: Linux truncates
# comm to 15 chars, so "carplay-wireless" can never match.
# Hence: match on the path via ps, and skip $$ / $PPID.
kill_path() {
  for p in $(ps | grep "$1" | grep -v grep | awk '{print $1}'); do
    [ "$p" = "$$" ] && continue
    [ "$p" = "$PPID" ] && continue
    kill -9 "$p" 2>/dev/null
  done
}
echo "[stack] start uptime=$(cut -d' ' -f1 /proc/uptime)"

# ============================== STEP -1: DISARM THE WATCHDOG ==============================
# THE BOX RESETS AT ~430s FROM BOOT, EVERY BOOT, REGARDLESS OF WORKLOAD. Measured five times
# (427s, 430s, ~431s with the full stack, ~431s on a BARE IDLE BOX with no stack at all, and once
# predicted in advance to the second). The interval is anchored to BOOT, not to anything we run.
#
# It is NOT starvation. The feeder's 20s cadence was measured at a perfect 20.008s from 3.4s uptime
# all the way to 423.58s — the last feed lands 4-7s BEFORE the reset. The feeder never misses.
#
# Mechanism (inferred, best fit for all five resets): the FEEDER ITSELF IS THE TRIGGER. The stock
# vendor rootfs never opens /dev/watchdog at all — the 20s feeder is an addition made by this
# project's own OCBM baseline rc.preboot. The first open at 3.4s ARMS the sunxi hardware watchdog;
# the subsequent pings do not actually restart the silicon counter; and the period really programmed
# is ~427s, not the 300s the driver prints. 3.4 + ~427 = ~430. Every boot.
#
# So the fix is to STOP feeding and disarm, which is the opposite of what this project's docs said.
# `nowayout=0`, so the magic-close ('V' then close) is permitted and stops the timer.
# ORDER MATTERS: kill the feeder FIRST — otherwise its next `echo 1 > /dev/watchdog` re-arms us.
# Permanent fix belongs in the next squashfs build: drop the feeder line from rc.preboot entirely.
disarm_watchdog() {
  # The feeder is `( while true; do echo 1 > /dev/watchdog; sleep 20; done ) &` — kill the SUBSHELL
  # (the sleep's parent), not just the sleep, or the loop simply spawns another.
  for _sp in $(ps | grep "[s]leep 20" | awk '{print $1}'); do
    _par=$(awk '{print $4}' /proc/$_sp/stat 2>/dev/null)
    [ -n "$_par" ] && [ "$_par" != 1 ] && kill -9 "$_par" 2>/dev/null
    kill -9 "$_sp" 2>/dev/null
  done
  usleep 300000
  echo V > /dev/watchdog 2>/dev/null && echo "[stack] watchdog: magic-close sent (disarmed)" \
    || echo "[stack] watchdog: could not write /dev/watchdog"
  echo "[stack] watchdog: feeder procs remaining=$(ps | grep -c '[s]leep 20')"
}
disarm_watchdog

# 0. Black-box recorder FIRST. The box has reset repeatedly during wireless activity and the kernel
#    persists nothing across a reset, so this is the only pre-crash evidence there is.
kill_path /tmp/c2air_blackbox.sh
setsid /tmp/c2air_blackbox.sh </dev/null >/tmp/bb.err 2>&1 &
echo "[stack] blackbox -> /mnt/UDISK/blackbox.log"

# 1. Wi-Fi AP. Must exist BEFORE carplay-wireless answers 0x5702, or the 0x5703 reply describes
#    an AP that isn't there. CARPLAY_HOSTAPD_CONF below MUST point at the file this generates.
/tmp/c2air_ap_up.sh start
echo "[stack] ap: $(/tmp/c2air_ap_up.sh status | tr '\n' ' ')"

# 2. Bluetooth line-discipline attach. hci0 dies with this fd, so it must outlive us.
kill_path /tmp/c2air-btattach; usleep 300000
setsid /tmp/c2air-btattach </dev/null >/tmp/bt.log 2>&1 &
i=0; while [ ! -e /sys/class/bluetooth/hci0 ] && [ $i -lt 50 ]; do i=$((i+1)); usleep 100000; done
echo "[stack] hci0: $([ -e /sys/class/bluetooth/hci0 ] && echo up || echo MISSING)"

# 3. Wireless CarPlay.
#    CARPLAY_STATE_DIR is on UDISK, not /tmp: BT link keys live there and /tmp is tmpfs, so a reboot
#    would silently drop the bond while the PHONE still has it — the phone then reconnects with a key
#    the box cannot answer. UDISK is nearly full (~316 KB) but a bond is a few hundred bytes.
# 3. Session supervisor — it, NOT this script, starts carplay-wireless.
#
# THE ORDERING FIX. Starting the wireless plane here (as this script used to) let a bonded iPhone
# reconnect and complete its whole AirPlay handshake BEFORE the host app subscribed. airplayd picks
# relay-vs-local ONCE at control-connection accept (airplayd main.rs:1386-1401) and ocbmd only
# attaches the RTSP seam while subscribed (ocbmd main.rs:2845-2851) — so that connection was locked
# to plain-local for life and no A/V could ever flow, however healthy everything downstream looked.
# The CCPA cannot hit this: `wireless_up` runs solely from the host-present 0->1 edge
# (session_supervisor.sh:788-791). c2air_supervisor.sh restores that gate.
kill_path /tmp/c2air_supervisor.sh; usleep 200000
kill_path /tmp/carplay-wireless
kill_path /tmp/airplayd
kill_path /tmp/rx-connect
setsid /tmp/c2air_supervisor.sh </dev/null >/tmp/sup.log 2>&1 &
usleep 1500000
echo "[stack] supervisor: $(ps | grep -c '[/]tmp/c2air_supervisor.sh') proc(s) — wireless starts on host_present 0->1"

# 4. Log snapshotter. Reading box logs otherwise costs the very session being diagnosed: the app
#    holds the USB interface, so `ocbm-host console` is blocked, and ADB is gone in accessory mode —
#    the only way in is to quit the app and reboot, which kills the session. This mirrors the tails
#    onto UDISK so a post-mortem needs no live link. Hard byte caps: UDISK has ~308 KB free and holds
#    the per-unit carplay.key, so this must never be allowed to fill it.
#    NOTE the wireless airplayd logs to airplayd_wl.log (av.rs:417), NOT airplayd.log.
mkdir -p /mnt/UDISK/snap
setsid sh -c 'while true; do
  for f in cw airplayd_wl rx-connect ocbmd stack ap_hostapd ap_dhcp; do
    [ -f /tmp/$f.log ] && tail -c 12000 /tmp/$f.log > /mnt/UDISK/snap/$f.log 2>/dev/null
  done
  cp /proc/net/arp /mnt/UDISK/snap/arp 2>/dev/null
  sync; sleep 10
done' </dev/null >/dev/null 2>&1 &
echo "[stack] log snapshotter -> /mnt/UDISK/snap (10s, capped 12 KB/file)"
echo "[stack] done"
