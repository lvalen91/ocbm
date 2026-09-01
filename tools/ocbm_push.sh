#!/bin/bash
# Push staged binaries to the box over OCBM (USB bulk) instead of the UART.
#
# WHY THIS EXISTS: uart_push.sh base64-encodes through a 115200-baud console. A 218 KB UPX-packed
# binary is ~292 KB of base64, which is ~25 s of pure line time at best and in practice stalls
# outright when anything else holds the port (docs/ops/04_OPEN_ITEMS.md). OCBM moves the same bytes over the bulk
# endpoints in about a second, CRC-checked and atomically renamed box-side by ocbmd's file_push.
#
# THE ADAPTER MUST BE PLUGGED INTO THIS MAC, not the head unit — this speaks USB to 1314:2d00.
# ocbmd must be running on the box to serve the transfer; it is the file_push handler.
#
#   tools/ocbm_push.sh                       # push the default staged pair, waiting for the device
#   tools/ocbm_push.sh <local> <remote> 755  # push one file
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HOST="$REPO/target/release/ocbm-host"
VID=1314 PID=2d00

[ -x "$HOST" ] || { echo "[ocbm-push] building ocbm-host"; ( cd "$REPO" && cargo build --release -p ocbm-host ); }

# Wait for the adapter so this can be started BEFORE the cable is moved over from the truck.
#
# Detection uses ioreg, NOT `system_profiler SPUSBDataType`: on this Mac the latter returns an empty
# document, so a poll built on it spins the full timeout and reports "no adapter" even when one is
# plugged in. ioreg -p IOUSB reports the bus correctly.
wait_for_device() {
    local waited=0
    until ioreg -p IOUSB -w0 -l 2>/dev/null | grep -qi "idProduct.*$((16#$PID))"; do
        [ "$waited" -eq 0 ] && echo "[ocbm-push] waiting for the adapter on USB (plug it into this Mac)..."
        sleep 2; waited=$((waited + 2))
        if [ "$waited" -ge 180 ]; then echo "[ocbm-push] FAIL: no 1314:$PID after ${waited}s"; return 1; fi
    done
    [ "$waited" -gt 0 ] && echo "[ocbm-push] adapter appeared after ${waited}s"
    return 0
}

push_one() {
    local local_f="$1" remote_f="$2" mode="${3:-755}"
    [ -f "$local_f" ] || { echo "[ocbm-push] FAIL: no such local file: $local_f"; return 1; }
    local sz; sz=$(stat -f%z "$local_f")
    echo "[ocbm-push] $local_f -> $remote_f ($sz bytes, mode $mode)"
    "$HOST" push "$VID" "$PID" "$local_f" "$remote_f" "$mode" || { echo "[ocbm-push] FAIL: $remote_f"; return 1; }
    # Verify box-side rather than trusting the transfer's own CRC: a correct CRC on a file written to
    # a full jffs2 still leaves a truncated binary, and the rootfs here runs ~4 MB free.
    local want got
    want=$(md5 -q "$local_f")
    got=$("$REPO/tools/ocbmcmd.py" "md5sum $remote_f" 2>/dev/null | grep -oE '^[0-9a-f]{32}' || true)
    if [ "$want" = "$got" ]; then echo "[ocbm-push] OK  md5 $got"; else
        echo "[ocbm-push] MD5 MISMATCH on $remote_f: local=$want box=${got:-<no reply>}"; return 1
    fi
}

# ---------------------------------------------------------------------------------------------
# DEPENDENCY GUARD: this script is a PUSH, not an install.
#
# session_supervisor.sh resolves its four radio call sites through the chipset-neutral seam
# (`sh /script/radio_hal.sh bt_on`, docs/wireless/01_BT_AND_RADIO.md) and invokes it inside a detached setsid wrapper whose
# EXIT STATUS IT NEVER READS. So a box that receives the supervisor without radio_hal.sh +
# radio_detect.sh has no radio bring-up at all, and says nothing about it: OCBM claims, MFi proves,
# CT_SUBSCRIBE succeeds, HOST_PRESENT arrives -- and Bluetooth silently never exists.
#
# That is not hypothetical. It is exactly the state this box was found in on 2026-08-28, reached by
# pushing the supervisor with this script while the seam had never been installed. Cost: a full
# debugging session, chasing hciattach's "Can't set line discipline: Invalid argument" three steps
# downstream of the real cause. See docs/ops/06_CORRECTIONS_LEDGER.md R-20W-5.
#
# ocbm_install.sh --full ships them together and already warns about this in prose. A warning in a
# file nobody opens while pushing is not a control, so check it here, where the mistake is made.
check_supervisor_deps() {
    case " $* " in *session_supervisor.sh*) ;; *) return 0 ;; esac
    local missing=""
    for f in radio_hal.sh radio_detect.sh; do
        "$REPO/tools/ocbmcmd.py" "test -e /script/$f && echo yes" 2>/dev/null | grep -q yes \
            || missing="$missing $f"
    done
    [ -z "$missing" ] && return 0
    echo ""
    echo "[ocbm-push] ================================ WARNING ================================"
    echo "[ocbm-push] You are pushing session_supervisor.sh to a box MISSING:$missing"
    echo "[ocbm-push]"
    echo "[ocbm-push] The supervisor brings radios up via 'sh /script/radio_hal.sh bt_on' and never"
    echo "[ocbm-push] reads the exit status. Without the seam there is NO radio bring-up and NO error"
    echo "[ocbm-push] anywhere — OCBM, MFi and CT_SUBSCRIBE all still succeed, and Bluetooth simply"
    echo "[ocbm-push] never comes up. This is docs/ops/06_CORRECTIONS_LEDGER.md R-20W-5, found the hard way."
    echo "[ocbm-push]"
    echo "[ocbm-push] Push these too, or use tools/ocbm_install.sh --full:"
    for f in $missing; do echo "[ocbm-push]     tools/ocbm_push.sh ccpa/rootfs/script/$f /script/$f 755"; done
    echo "[ocbm-push] =========================================================================="
    echo ""
    return 1
}

wait_for_device
check_supervisor_deps "$@" || true   # warn loudly; do not block a deliberate partial push

if [ $# -ge 2 ]; then
    push_one "$@"
else
    push_one /tmp/boxbins/carplay-wireless /usr/sbin/carplay-wireless 755
    push_one /tmp/boxbins/ocbmd           /usr/sbin/ocbmd           755
    echo "[ocbm-push] both landed. Restart on the box:  killall ocbmd carplay-wireless"
fi
