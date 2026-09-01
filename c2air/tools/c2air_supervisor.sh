#!/bin/sh
# c2air_supervisor.sh — the C2Air's minimal session supervisor. Runs ON THE BOX.
#
# ============================ WHY THIS EXISTS (the bug it fixes) ============================
# This is the single most important script in the C2Air port, and its absence was why wireless
# CarPlay reached PAIR-VERIFY COMPLETE and then produced no A/V at all.
#
# Two facts from the SHARED (CCPA-proven) Rust:
#
#   1. `RS_OPEN` is emitted when airplayd ACCEPTS a control connection, not at pair-verify
#      (crates/vendor/receiver/src/relay.rs:674-697, ccpa/airplayd/src/main.rs:1389).
#   2. The relay-vs-local DELEGATE IS CHOSEN ONCE, AT ACCEPT, from `appsetup() && seam_up()`
#      (ccpa/airplayd/src/main.rs:1386-1401). A control connection accepted while the RTSP seam is
#      down is PERMANENTLY plain-local — it never gets an RS_OPEN later, so the host app can never
#      author SETUP for it, so no A/V ever flows on that connection.
#
# And ocbmd only attaches that seam while SUBSCRIBED (ccpa/ocbmd/src/main.rs:2845-2851), i.e. only
# after the host app has connected and sent CT_SUBSCRIBE.
#
# On the CCPA this ordering CANNOT be violated: `wireless_up` is invoked solely from the
# host-present 0->1 edge (tools/session_supervisor.sh:788-791), so carplay-wireless — and therefore
# airplayd, and therefore any phone control connection — cannot exist before a subscribed host.
#
# The first C2Air bring-up inverted that: the stack started AP + BT + carplay-wireless AT BOOT,
# because switching the USB gadget to accessory mode removes ADB and a shell was needed beforehand.
# A bonded iPhone then reconnected immediately and completed its whole AirPlay handshake on a
# connection accepted with the seam down — locked to plain-local for life. Everything downstream
# looked perfect (bond, iAP2, 0x5703, DHCP, pair-verify, MFi-SAP) and not one frame could ever move.
#
# So: THE WIRELESS PLANE MUST NOT COME UP UNTIL THE HOST HAS SUBSCRIBED. That is all this does.
#
# `/tmp/host_present` is written by ocbmd (ccpa/ocbmd/src/main.rs:1250) — 1 while a subscribed host
# is present, 0 otherwise. Watching its edges is exactly what the CCPA supervisor does.
#
# ============================ SCOPE: deliberately much smaller than the CCPA's ==============
# tools/session_supervisor.sh also runs the WIRED path (projection_up/iap2d), dual-transport
# arbitration, an escalation ladder, and app-command actuators. None of that is ported here:
#   * The C2Air is WIRELESS-ONLY for now — there is no wired iPhone path on this board.
#   * `/tmp/phone_present` is deliberately NOT written: it is a probe for a genuine wired iPhone
#     enumerated on the USB bus (Apple VID 05ac) and is "ALWAYS false for a wireless-only session"
#     (session_supervisor.sh:86-92). Writing it here would be a lie.
# What IS ported is the part that is load-bearing for correctness: the presence-edge gate and the
# clean reap of stale A/V children before a fresh bring-up (session_supervisor.sh:528-534, whose
# comment records that skipping it produces "connection unsuccessful" on the next attempt).
#
# Usage (on the box):  setsid /tmp/c2air_supervisor.sh </dev/null >/tmp/sup.log 2>&1 &
set -u
# /tmp/bin FIRST: it holds a `pkill` shim. VERIFIED MISSING on this rootfs — `pgrep` and `killall`
# have /usr/bin symlinks but `pkill` does NOT, while crates/vendor/wireless/src/av.rs shells out to a
# bare `pkill` at :185, :193, :334 and :427 to reap airplayd/rx-connect. Every one of those spawns
# fails with ENOENT, so stale A/V children are never reaped and the next session collides with them.
# busybox HAS the applet; it just has no symlink. (pgrep resolving is why detection worked and this
# stayed hidden.)
export PATH=/tmp/bin:/usr/sbin:/usr/bin:/sbin:/bin:$PATH
mkdir -p /tmp/bin
[ -e /tmp/bin/pkill ] || ln -sf "$(command -v busybox || echo /bin/busybox)" /tmp/bin/pkill
export LD_LIBRARY_PATH=/tmp/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}

HOST_FLAG=/tmp/host_present
CW=/tmp/carplay-wireless

# Kill by full path, skipping ourselves and our parent.
#
# NEITHER pkill form is usable on this box and both have already caused real failures:
#   * `pkill -x carplay-wireless` can never match — Linux truncates comm to 15 chars
#     ("carplay-wireles"), so the old daemon survives, two instances fight over RFCOMM ch 1, and
#     every pairing fails with "RFCOMM accept error: Address in use".
#   * `pkill -f <path>` matches FULL command lines including the invoking shell, so it kills its own
#     parent and takes this script down mid-bring-up.
kill_path() {
  for _p in $(ps | grep "$1" | grep -v grep | awk '{print $1}'); do
    [ "$_p" = "$$" ] && continue
    [ "$_p" = "$PPID" ] && continue
    kill -9 "$_p" 2>/dev/null
  done
}

wireless_running() { ps | grep -v grep | grep -q "$CW"; }

wireless_up() {
  wireless_running && { echo "[sup] wireless already up — idempotent no-op"; return; }

  # Clean slate. Ported from session_supervisor.sh:528-534: reap orphaned A/V children from a prior
  # session so the new stack can never latch onto or collide with a stale airplayd / rx-connect.
  kill_path /tmp/airplayd
  kill_path /tmp/rx-connect
  sleep 1   # let the stragglers actually die before carplay-wireless spawns its own

  echo "[sup] host present -> bringing wireless up at uptime=$(cut -d' ' -f1 /proc/uptime)"
  # CARPLAY_HOSTAPD_CONF must name the SAME file c2air_ap_up.sh generated, or the 0x5703 reply
  # describes an AP that does not exist. State + peers go to UDISK because / is a read-only squashfs.
  CARPLAY_HCI_BACKEND=native \
  CARPLAY_HOSTAPD_CONF=/tmp/hostapd.conf \
  CARPLAY_STATE_DIR=/mnt/UDISK/cpstate \
  PEERSTORE_PATH=/mnt/UDISK/cpstate/carplay_peers.bin \
  AIRPLAYD_BIN=/tmp/airplayd \
  RX_CONNECT_BIN=/tmp/rx-connect \
    setsid "$CW" </dev/null >/tmp/cw.log 2>&1 &
  sleep 2
  wireless_running && echo "[sup] carplay-wireless up" || echo "[sup] ERROR: carplay-wireless failed to start"
}

wireless_down() {
  echo "[sup] host gone -> tearing wireless down at uptime=$(cut -d' ' -f1 /proc/uptime)"
  kill_path "$CW"
  kill_path /tmp/airplayd
  kill_path /tmp/rx-connect
}

echo "[sup] c2air supervisor started; watching $HOST_FLAG (wireless gated on the 0->1 edge)"
last_p=""
while true; do
  # Absent flag == no ocbmd yet, or ocbmd restarted (it writes 0 at startup, ocbmd main.rs:2482).
  if [ -r "$HOST_FLAG" ]; then p=$(cat "$HOST_FLAG" 2>/dev/null); else p=0; fi
  case "$p" in 0|1) ;; *) p=0 ;; esac

  if [ "$p" = 1 ] && [ "$last_p" != 1 ]; then
    wireless_up
  fi
  if [ "$p" = 0 ] && [ "$last_p" = 1 ]; then
    wireless_down
  fi
  last_p="$p"
  sleep 1
done
