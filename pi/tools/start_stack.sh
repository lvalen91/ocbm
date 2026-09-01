#!/usr/bin/env bash
# Start the wireless-CarPlay accessory stack on the Raspberry Pi (AAOS).
#
# This encodes the ONE launch environment the stack needs. Every variable below is an opt-in gate:
# unset, the binaries behave exactly as they do on a CCPA, which is why they can be shared.
#
# Run from the Mac:  pi/tools/start_stack.sh [--serial <adb-serial>] [--restart]
#
# WARNING: --restart kills any live CarPlay session. Without it this refuses to start a second
# carplay-wireless, because two processes fighting over hci0 is a confusing failure rather than an
# obvious one.

set -euo pipefail

SERIAL=""
RESTART=0
while [ $# -gt 0 ]; do
    case "$1" in
        --serial)  SERIAL="$2"; shift ;;
        --restart) RESTART=1 ;;
        -h|--help) sed -n '2,12p' "$0"; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
    shift
done

adb=(adb)
[ -n "$SERIAL" ] && adb=(adb -s "$SERIAL")
say() { printf '\033[1m==>\033[0m %s\n' "$*"; }

TMP=/data/local/tmp
APPFILES=/data/user/10/com.carlink.projection/files

# ---- the launch environment -------------------------------------------------------------------
#
# CARPLAY_HCI_BACKEND=native
#   hciconfig is BlueZ userspace and does not exist on Android. Uses raw HCI rather than mgmt,
#   because mgmt synthesises the EIR itself and cannot express the CarPlay marker UUID.
#
# CARPLAY_RFCOMM_BACKEND=userspace
#   The AAOS kernel ships without CONFIG_BT_RFCOMM, so the RFCOMM socket family returns
#   EPROTONOSUPPORT. Android's own stack implements RFCOMM in userspace over L2CAP for the same
#   reason, so this is not a workaround so much as the same decision.
#
# CARPLAY_MFI_ADDR
#   This host has no MFi coprocessor. Cert/sign go over USB-NCM to mfid on the CCPA, which is
#   reduced to an MFi oracle. Measured: cert 155-165 ms, sign ~1470 ms, well inside the phone's
#   10 s per-operation timeout.
#
# CARPLAY_STATE_DIR / PEERSTORE_PATH
#   /etc is a symlink into the read-mostly /system partition, so BT link keys and the peer store
#   must live under /data.
#
# CARPLAY_CFG_FILE
#   THE ONE THAT IS EASY TO MISS. Points airplayd at the config the projection app generates from
#   the live display. Without it airplayd falls back to its compiled default, which advertises
#   H.264 — and the app's decoder is HEVC-only, so every frame decrypts perfectly and NOTHING
#   RENDERS, with healthy counters everywhere. The app logs that mismatch explicitly; this is the
#   fix for it.
#
#   airplayd is spawned BY carplay-wireless, and Rust's Command inherits the parent environment,
#   so setting it here reaches airplayd without any extra plumbing.
ENV_LINE="\
CARPLAY_HCI_BACKEND=native \
CARPLAY_RFCOMM_BACKEND=userspace \
CARPLAY_MFI_ADDR=192.168.50.2:7789 \
CARPLAY_HOSTAPD_CONF=$TMP/hostapd_5g.conf \
CARPLAY_STATE_DIR=$TMP/carplay \
PEERSTORE_PATH=$TMP/carplay/carplay_peers.bin \
CARPLAY_CFG_FILE=$APPFILES/carplay_cfg.yaml \
AIRPLAYD_BIN=$TMP/airplayd \
RX_CONNECT_BIN=$TMP/rx-connect"

# ---- preflight ---------------------------------------------------------------------------------

# `pgrep -x carplay-wireless` MATCHES NOTHING, and finding that out the hard way cost a
# split-brain stack on the bench.
#
# The kernel's TASK_COMM_LEN is 16 bytes including the NUL, so `comm` is truncated at 15
# characters: /proc/<pid>/comm reads `carplay-wireles`. toybox pgrep -x compares against comm, so
# the exact-match never fires, the preflight below concluded "nothing running", and a SECOND
# carplay-wireless started against a controller the first one already owned. The newcomer then
# looped forever on `RFCOMM accept error: Address already in use` while the original kept the
# session — working, but with two owners and no indication which.
#
# Same family as the `pgrep -x` trap already recorded in pi/docs/00 §4 (BusyBox matches argv[0],
# toybox matches comm). Match on the truncated name via -f against the full command line instead,
# which is unambiguous here because the binary is invoked by path.
running=$("${adb[@]}" shell 'pgrep -f "[c]arplay-wireless" 2>/dev/null | tr -d "\r"' || true)
if [ -n "$running" ]; then
    if [ "$RESTART" -eq 0 ]; then
        say "carplay-wireless is already running (pid $running)."
        say "Refusing to start a second one — two owners of hci0 fail confusingly."
        say "Use --restart to replace it. THIS DROPS ANY LIVE CARPLAY SESSION."
        exit 1
    fi
    say "stopping the running stack (pids: $(echo $running | tr '\n' ' '))"
    # Every pid, not just the first: a split-brain bench can already have more than one.
    for pid in $running; do
        "${adb[@]}" shell "kill $pid" || true
    done
    # airplayd and rx-connect double-fork to init, so killing the parent does NOT reap them —
    # and a surviving rx-connect holds L2CAP PSM 3, which makes the NEXT start fail with
    # "Address already in use" (pi/docs/00 §4, the cloexec leak).
    "${adb[@]}" shell 'pkill -f "[a]irplayd"; pkill -f "[r]x-connect"' || true
    "${adb[@]}" shell 'sleep 2'

    still=$("${adb[@]}" shell 'pgrep -f "[c]arplay-wireless" 2>/dev/null | tr -d "\r"' || true)
    if [ -n "$still" ]; then
        say "REFUSING TO START: carplay-wireless is still alive (pids: $(echo $still | tr '\n' ' '))."
        say "Two owners of hci0 is the confusing failure this check exists to prevent."
        exit 1
    fi
fi

say "checking the generated config is present"
if ! "${adb[@]}" shell "test -f $APPFILES/carplay_cfg.yaml && echo yes" | grep -q yes; then
    say "WARNING: $APPFILES/carplay_cfg.yaml does not exist."
    say "Start the projection app first (it writes the config at service start):"
    say "  adb shell am start -n com.carlink.projection/.LaunchTileActivity"
    say "Continuing anyway — airplayd will log that it fell back to compiled defaults."
fi

# Android deletes the `from all lookup main` rule, so the connected route for the AP subnet exists
# in `main` the whole time but nothing consults it: any process outside the framework gets
# ENETUNREACH to its own AP subnet. Scoped to the CarPlay subnet only.
say "installing the CarPlay policy routing rules"
"${adb[@]}" shell 'ip rule add to 192.168.43.0/24 lookup main pref 15000 2>/dev/null; \
                   ip rule add from 192.168.43.1 lookup main pref 15001 2>/dev/null; true'

# ---- start -------------------------------------------------------------------------------------

say "starting carplay-wireless"
# `adb shell "... &" &` does NOT detach: adb keeps the shell's stdout open, so the local adb
# client blocks until the daemon exits — which is never. Observed as this script hanging for
# 10 minutes with the stack running perfectly the whole time.
#
# nohup + closing all three standard fds inside the device shell is what actually releases adb.
"${adb[@]}" shell "cd $TMP && $ENV_LINE nohup ./carplay-wireless > $TMP/wireless.log 2>&1 < /dev/null &"
sleep 3

say "running processes:"
"${adb[@]}" shell 'ps -A -o PID,ARGS 2>/dev/null | grep -E "carplay-wireless|airplayd|hostapd|apdhcpd|rx-connect" | grep -v grep' || true

cat <<EOF

Watch the stack:      adb shell tail -f $TMP/wireless.log
Watch the app:        adb logcat -s NETPROBE
Confirm HEVC:         adb logcat -s NETPROBE | grep "video config"
                      (an 'avcC' there means CARPLAY_CFG_FILE did not take effect)
EOF
