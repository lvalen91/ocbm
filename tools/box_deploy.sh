#!/bin/bash
# One-command box deploy: latch on wherever the box is, flip it to NCM, push over scp, put it back.
#
# WHY THIS EXISTS. Deploying a box binary used to be a manual mode flip: open the macOS app, click
# "Enter NCM maintenance mode", CLOSE the app so it releases the USB device, wait, then push — and
# the push itself went over telnet, because that is what `ncm_base_install.sh` defaults to. Telnet is
# the EMERGENCY RESCUE PATH: it exists so a box with broken ssh can be repaired. Routing a 1.8 MB
# binary through it (2 KB base64 chunks over a line-oriented shell) is using the lifeboat as a ferry.
#
# What made this scriptable was one missing piece: `MGMT_ENTER_NCM` has been in `ocbm-proto` and
# handled by `ocbmd` since the CCPA tab shipped, but only the macOS app could SEND it. `ocbm-host ncm`
# closes that gap, so the whole cycle is now:
#
#     OCBM (accessory) --ocbm-host ncm--> reboot --> NCM (ssh) --scp--> install --[--return]--> OCBM
#
# TRANSPORT NOTE, measured 2026-09-05. `scp -O` moves 1.8 MB over the USB-NCM link in ~0.6 s,
# md5-identical. A single-shot `busybox nc` on the same link truncated the same file at 380 KB and
# again at 432 KB — different points, so it is the tool and the backgrounded listener dying with its
# telnet session, NOT the link. Do not "optimise" this back to nc.
#
# USAGE
#   tools/box_deploy.sh [--return] [--dry-run] SRC:DEST[:MODE] [SRC:DEST[:MODE] ...]
#   tools/box_deploy.sh --return-only            # just put a box in NCM back into accessory mode
#
#   --return      after installing, clear /script/ncm_only and reboot back to OCBM, then wait for USB
#   --dry-run     resolve mode + verify reachability + print the plan; transfer nothing
#   MODE          octal chmod, default 755
#
# EXAMPLE
#   tools/box_deploy.sh --return \
#     target/armv7-unknown-linux-musleabihf/release/airplayd:/usr/sbin/airplayd
#
# The box is reachable as ssh host `ccpa-ncm` (see ~/.ssh/config). Key auth only; no password, and
# no `PubkeyAuthentication=no` — dropbear here has no root password set, so forcing password auth is
# how you get a confusing "Permission denied" on a box that is working perfectly.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HOST_BIN="$REPO/target/release/ocbm-host"
BOX_SSH="ccpa-ncm"
VID=1314 PID=2d00
NCM_WAIT=90        # seconds to wait for ssh after ENTER_NCM (the reboot is normally a few seconds)
OCBM_WAIT=120      # seconds to wait for the USB device after returning

RETURN=0 DRYRUN=0 RETURN_ONLY=0
FILES=()
while [ $# -gt 0 ]; do
  case "$1" in
    --return)      RETURN=1 ;;
    --dry-run)     DRYRUN=1 ;;
    --return-only) RETURN_ONLY=1; RETURN=1 ;;
    -h|--help)     sed -n '1,40p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *)             FILES+=("$1") ;;
  esac
  shift
done

say()  { printf '[deploy] %s\n' "$*"; }
die()  { printf '[deploy] ERROR: %s\n' "$*" >&2; exit 1; }

ssh_ok()  { ssh -o ConnectTimeout=4 -o BatchMode=yes "$BOX_SSH" true 2>/dev/null; }
# ioreg, not `system_profiler SPUSBDataType` — the latter returns an empty document on this Mac, so a
# poll built on it spins the full timeout and reports "no adapter" with one plugged in. Same reasoning
# as tools/ocbm_push.sh.
usb_ok()  { ioreg -p IOUSB -w0 -l 2>/dev/null | grep -qi "idProduct.*$((16#$PID))"; }

wait_for() { # wait_for <predicate-fn> <seconds> <label>
  local fn="$1" limit="$2" label="$3" waited=0
  while ! "$fn"; do
    sleep 2; waited=$((waited + 2))
    [ "$waited" -ge "$limit" ] && return 1
    [ $((waited % 10)) -eq 0 ] && say "still waiting for $label (${waited}s)"
  done
  return 0
}

# ---- 1. Where is the box? ------------------------------------------------------------------------
# Order matters: check NCM FIRST. A box already in maintenance mode must not be sent an ENTER_NCM it
# cannot receive (ocbmd is not running in NCM), and re-flipping a box that is already there would
# reboot it for nothing.
MODE=""
if ssh_ok; then
  MODE=ncm
  say "box is in NCM maintenance mode (ssh $BOX_SSH)"
elif usb_ok; then
  MODE=ocbm
  say "box is on USB in accessory mode (OCBM $VID:$PID)"
else
  die "box not found — neither ssh '$BOX_SSH' nor a USB device $VID:$PID. Plug it into this Mac."
fi

if [ "$RETURN_ONLY" -eq 0 ] && [ ${#FILES[@]} -eq 0 ]; then
  die "nothing to deploy — pass SRC:DEST pairs, or --return-only to just exit NCM"
fi

# Validate every source BEFORE touching the box: discovering a typo after a reboot wastes a cycle.
for spec in "${FILES[@]:-}"; do
  [ -z "$spec" ] && continue
  src="${spec%%:*}"; rest="${spec#*:}"; dest="${rest%%:*}"
  [ -f "$src" ]  || die "no such file: $src"
  [ "$dest" = "$src" ] && die "malformed spec '$spec' — expected SRC:DEST[:MODE]"
  case "$dest" in /*) ;; *) die "destination must be absolute: $dest" ;; esac
done

if [ "$DRYRUN" -eq 1 ]; then
  say "DRY RUN — mode=$MODE, return=$RETURN"
  for spec in "${FILES[@]:-}"; do
    [ -z "$spec" ] && continue
    src="${spec%%:*}"; rest="${spec#*:}"; dest="${rest%%:*}"; mode="${rest#*:}"
    [ "$mode" = "$dest" ] && mode=755
    printf '[deploy]   %s -> %s (mode %s, %s bytes)\n' "$src" "$dest" "$mode" "$(wc -c < "$src" | tr -d ' ')"
  done
  exit 0
fi

# ---- 2. Flip to NCM if needed --------------------------------------------------------------------
if [ "$MODE" = ocbm ]; then
  [ -x "$HOST_BIN" ] || { say "building ocbm-host"; ( cd "$REPO" && cargo build --release -p ocbm-host >/dev/null ); }
  say "commanding ENTER_NCM over OCBM"
  # The app holds the USB device exclusively; if it is running, this fails with a claim error. Say so
  # plainly rather than letting the user read a libusb errno.
  "$HOST_BIN" ncm "$VID" "$PID" || die "ENTER_NCM failed — is the macOS app running? It holds the USB device."
  say "waiting for the box to come back on NCM (up to ${NCM_WAIT}s)"
  wait_for ssh_ok "$NCM_WAIT" "ssh $BOX_SSH" || die "box did not reappear on ssh within ${NCM_WAIT}s"
  say "box is up in NCM"
fi

# ---- 3. Transfer + install -----------------------------------------------------------------------
# Every file: scp to /tmp (tmpfs — costs no rootfs space while the old copy still exists), verify the
# md5 box-side against the local one, then move into place. The verify is not ceremony: it is what
# caught the silent nc truncation, and a half-written daemon on the box is a recovery job.
for spec in "${FILES[@]:-}"; do
  [ -z "$spec" ] && continue
  src="${spec%%:*}"; rest="${spec#*:}"; dest="${rest%%:*}"; mode="${rest#*:}"
  [ "$mode" = "$dest" ] && mode=755
  base="$(basename "$dest")"
  want="$(md5 -q "$src")"
  say "pushing $(basename "$src") -> $dest ($(wc -c < "$src" | tr -d ' ') bytes)"
  scp -O -q "$src" "$BOX_SSH:/tmp/.deploy.$base"
  got="$(ssh "$BOX_SSH" "md5sum /tmp/.deploy.$base 2>/dev/null | cut -d' ' -f1")"
  [ "$got" = "$want" ] || {
    ssh "$BOX_SSH" "rm -f /tmp/.deploy.$base" || true
    die "md5 mismatch for $dest (local $want, box $got) — NOT installing"
  }
  # Keep the outgoing copy in tmpfs as a same-session rollback. It does NOT survive a reboot, which is
  # deliberate: after a reboot the flash copy IS the new one and a stale .prev would be a trap.
  ssh "$BOX_SSH" "
    set -e
    [ -f '$dest' ] && cp '$dest' '/tmp/.prev.$base' || true
    rm -f '$dest'
    cp '/tmp/.deploy.$base' '$dest'
    chmod $mode '$dest'
    rm -f '/tmp/.deploy.$base'
    sync
  "
  final="$(ssh "$BOX_SSH" "md5sum '$dest' | cut -d' ' -f1")"
  [ "$final" = "$want" ] || die "post-install md5 mismatch for $dest — box has $final"
  say "  installed, md5 verified (rollback this session: /tmp/.prev.$base)"
done

ssh "$BOX_SSH" 'df / | tail -1' | awk '{printf "[deploy] rootfs: %s used, %s free (%s)\n", $3, $4, $5}'

# ---- 4. Return to accessory mode -----------------------------------------------------------------
if [ "$RETURN" -eq 1 ]; then
  say "returning the box to accessory mode (rm /script/ncm_only; reboot)"
  # The reboot kills the connection, so a non-zero exit here is expected and not a failure.
  ssh "$BOX_SSH" 'rm -f /script/ncm_only; sync; (sleep 1; reboot) >/dev/null 2>&1 &' || true
  say "waiting for the box on USB-OCBM (up to ${OCBM_WAIT}s)"
  sleep 5
  if wait_for usb_ok "$OCBM_WAIT" "USB $VID:$PID"; then
    say "box is back on USB in accessory mode"
  else
    say "WARNING: box did not reappear on USB within ${OCBM_WAIT}s — check it before assuming a bad deploy"
    exit 1
  fi
fi

say "done"
