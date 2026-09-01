#!/usr/bin/env bash
# run_mfid.sh — stage and run the ephemeral MFi daemon on an NCM-mode CCPA, then take it away again.
#
# WHAT THIS IS
#   A bring-up harness, not an installer. `mfid` is copied to /tmp on the box (tmpfs), run in the
#   FOREGROUND of an ssh session this script holds open, and killed when you Ctrl-C. Nothing is
#   written to flash, no init/inittab/`/script` file is touched, and a reboot erases every trace.
#
# THE SIGHUP RULE (docs: ccpa-test-adapter-realtek "Transport gotchas")
#   Anything backgrounded over ssh/telnet on this box dies of SIGHUP when the session closes, and
#   `setsid` does NOT help — that is what silently truncated earlier nc transfers. So the daemon is
#   deliberately NOT backgrounded on the box. It runs in the foreground of a session held open here,
#   which is the pattern that actually works.
#
# WHERE TO RUN IT
#   Wherever the box is reachable at $BOX. With the CCPA on the Mac's USB that is the Mac; with the
#   CCPA on the Pi, run this on the Pi (or route to 192.168.50.0/24 through it).
#
# USAGE
#   tools/run_mfid.sh --build            build, deploy, run (Ctrl-C to stop and clean up)
#   tools/run_mfid.sh --status           is it running on the box?
#   tools/run_mfid.sh --stop             kill it and remove /tmp/mfid
#   tools/run_mfid.sh --box 192.168.50.2 --port 7789 --idle 900

set -euo pipefail
cd "$(dirname "$0")/.."

BOX="192.168.50.2"
PORT="7789"
IDLE="900"
MAXLIFE="0"
DO_BUILD=0
ACTION="run"
TARGET="armv7-unknown-linux-musleabihf"
BIN="target/${TARGET}/release/mfid"
REMOTE="/tmp/mfid"

while [ $# -gt 0 ]; do
  case "$1" in
    --box)    BOX="$2"; shift 2 ;;
    --port)   PORT="$2"; shift 2 ;;
    --idle)   IDLE="$2"; shift 2 ;;
    --max)    MAXLIFE="$2"; shift 2 ;;
    --build)  DO_BUILD=1; shift ;;
    --stop)   ACTION="stop"; shift ;;
    --status) ACTION="status"; shift ;;
    -h|--help) sed -n '2,23p' "$0"; exit 0 ;;   # header comment block only — stop before the code
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

# Root's password is blank on this box, so sshpass makes it non-interactive. Plain ssh still works
# if sshpass is absent — it will just prompt (press Enter).
if command -v sshpass >/dev/null 2>&1; then
  SSH=(sshpass -p '' ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null)
  SCP=(sshpass -p '' scp -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null)
else
  echo "note: sshpass not found — ssh will prompt for a password (it is blank; press Enter)" >&2
  SSH=(ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null)
  SCP=(scp -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null)
fi

box() { "${SSH[@]}" -n "root@${BOX}" "$@"; }

case "$ACTION" in
  status)
    # Both queries are shielded from `set -e` AND report transport failure. Previously the first
    # was silently shielded and the second was not, so an unreachable box printed nothing useful
    # and exited 255 mid-status.
    if staged=$(box "[ -e ${REMOTE} ] && echo staged || echo absent" 2>/dev/null); then
      echo "binary:          ${staged}"
    else
      echo "cannot reach ${BOX} over ssh — is the box in NCM mode and the link up?" >&2
      exit 1
    fi
    if procs=$(box "ps 2>/dev/null | grep -c '[m]fid' || true" 2>/dev/null); then
      echo "mfid processes:  ${procs:-0}"
    else
      echo "cannot query processes on ${BOX}" >&2
      exit 1
    fi
    exit 0
    ;;
  stop)
    echo "stopping mfid on ${BOX}"
    box "killall mfid 2>/dev/null; rm -f ${REMOTE}; true"
    echo "stopped and removed ${REMOTE}"
    exit 0
    ;;
esac

if [ "$DO_BUILD" = "1" ]; then
  echo "== building ${TARGET} =="
  export PATH="$HOME/.cargo/bin:$PATH"
  cargo zigbuild --target "$TARGET" --release -p mfid
fi

[ -f "$BIN" ] || { echo "missing $BIN — run with --build" >&2; exit 1; }

echo "== preflight =="
# Fail early and specifically rather than letting scp time out with a vague error.
#
# `-W` means MILLISECONDS on macOS (BSD ping) but SECONDS on Linux (iputils), and `-w` exists only
# on Linux. Getting that backwards is not cosmetic: `-W 2000` on the Pi waits up to 2000 SECONDS
# for a reply, so a box that is routed-but-silent (mid-reboot, wrong mode, or reached through the
# Pi per the header above) would hang this "fail early" check for ~33 minutes — on precisely the
# platform the check exists for.
case "$(uname -s)" in
  Darwin) PING=(ping -c 1 -W 2000) ;;   # -W is milliseconds here
  *)      PING=(ping -c 1 -W 2 -w 2) ;; # -W is seconds; -w caps total runtime
esac
"${PING[@]}" "$BOX" >/dev/null 2>&1 \
  || { echo "cannot reach ${BOX} — is the box in NCM mode and the USB link up?" >&2; exit 1; }

# Which boot flag is present decides whether mfid will even agree to start; report it here so a
# refusal is not a surprise 10 seconds later. start_main_service.sh:18 boots NCM on EITHER flag and
# OCBM only when neither exists, and `ncm_base_install.sh --wifi-backstop` rests on ncm_wifi with
# ncm_only removed — so checking only ncm_only would cry wolf on a valid NCM box.
set +e
box "[ -e /script/ncm_only ] || [ -e /script/ncm_wifi ]"
flag_rc=$?
set -e
case "$flag_rc" in
  0) echo "box is in NCM mode (ncm_only or ncm_wifi present) — good" ;;
  1) echo "WARNING: neither /script/ncm_only nor /script/ncm_wifi exists — this box boots OCBM" >&2
     echo "         and mfid will refuse to start." >&2
     echo "         Flip with: touch /script/ncm_only; sync; reboot   (~50 s)" >&2 ;;
  # Anything else is ssh failing (255), not a mode answer. Saying "you are in OCBM mode" here would
  # send the operator to reboot a box that actually just has a connectivity problem.
  *) echo "WARNING: could not read the boot flags over ssh (exit ${flag_rc}) — that is a" >&2
     echo "         connectivity problem, not a mode problem. Not advising a mode flip." >&2 ;;
esac

echo "== staging $(wc -c < "$BIN") bytes to ${BOX}:${REMOTE} =="
# Plain scp (SFTP mode). NEVER `scp -O`: the legacy protocol needs an scp binary on the remote and
# this box has none — /usr/libexec/sftp-server is what is actually present.
"${SCP[@]}" "$BIN" "root@${BOX}:${REMOTE}"
box "chmod +x ${REMOTE}"

cleanup() {
  # Run exactly once. On Ctrl-C the INT trap fires, then ssh's nonzero status trips `set -e` and
  # the EXIT trap would fire a SECOND full teardown (another ssh round-trip and banner).
  trap - EXIT INT TERM
  echo ""
  echo "== cleaning up =="
  box "killall mfid 2>/dev/null; rm -f ${REMOTE}; true" || true
  echo "mfid stopped and ${REMOTE} removed — nothing persists on the box."
}
trap cleanup EXIT INT TERM

echo "== running (Ctrl-C to stop) =="
echo "   probe it with: target/release/mfi-probe --addr ${BOX}:${PORT} selftest"
echo ""

# Foreground on the box, inside a session this script holds open. Do NOT background it remotely.
"${SSH[@]}" "root@${BOX}" \
  "${REMOTE} --bind 0.0.0.0:${PORT} --idle-timeout ${IDLE} --max-lifetime ${MAXLIFE}"
