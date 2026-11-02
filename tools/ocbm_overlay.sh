#!/usr/bin/env bash
# ocbm_overlay.sh — build the OCBM overlay image for the mainline baseline (baseline/README.md §2.8, §7.0e).
#
# The baseline rootfs is a GPL-3.0 image built by baseline/build.sh from pinned upstream sources; this
# repository's own daemons (Unlicense Rust: ocbmd, btd, carplayd) and the stock OCBM scripts are NOT part of
# it. They travel as this separate image, written into the last 0x460000 bytes of the unit's 'rootfs' MTD
# (rootfs+0x800000) and unpacked into RAM at boot by the baseline's `ocbm-overlay` — whose header format this
# script produces:
#
#   4096-byte header: "OCBM-OVERLAY 1\n" "payload_bytes=<n>\n" "payload_sha256=<hex>\n" "version=<text>\n", NUL pad
#   payload:          gzip'd ustar archive, unpacked at /opt/ocbm:
#                       bin/ocbmd-launch          the command the baseline's relay spawns (required)
#                       bin/ocbmd [btd carplayd]  target/armv7-unknown-linux-musleabihf/release
#                       etc/rc.ocbm etc/*.conf    ccpa/overlay/etc (boot hook, AP templates)
#                       script/*                  --full: the stock supervisor + radio seam, unmodified copies
#                       etc/MANIFEST              sha256 of every file above
#
# usage: tools/ocbm_overlay.sh [--minimal|--full] [--out FILE]
#   --minimal  ocbmd only (link, MFi, console, file, log, settime, heartbeat): the Stage-A image
#   --full     (default) + btd, carplayd, session_supervisor.sh, projection_up.sh, radio_detect.sh,
#              radio_hal.sh, radio_ap_up.sh: the app-driven wireless stack (Stage B)
# Output: ~/.cache/carlink-artifacts/ocbm-overlay/ocbm-overlay-<profile>-<stamp>.img (+ .sha256), never under
# ~/Documents (CLAUDE.md). Install on the unit (rootfs, any USB mode): get the file into /tmp (UART:
# baseline/common/tools/uart_xmodem_send.py into `rx`; NCM: scp -O), then
#   ocbm-overlay write /tmp/<file> <sha256>     (0 written, 3 already there, 4 refused; the next boot uses it)
set -euo pipefail
export LC_ALL=C COPYFILE_DISABLE=1
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ARM="$REPO/target/armv7-unknown-linux-musleabihf/release"
OV="$REPO/ccpa/overlay"
PROFILE=full
OUT=""
REGION=4587520 # board.env OVERLAY_SIZE (0x460000)
while [ $# -gt 0 ]; do
  case "$1" in
    --minimal) PROFILE=minimal; shift ;;
    --full) PROFILE=full; shift ;;
    --out) OUT="$2"; shift 2 ;;
    -h|--help) sed -n '2,30p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
STAMP=$(date -u +%Y%m%dT%H%MZ)
[ -n "$OUT" ] || OUT="$HOME/.cache/carlink-artifacts/ocbm-overlay/ocbm-overlay-$PROFILE-$STAMP.img"
case "$OUT" in "$HOME/Documents"/*) echo "refusing to write under ~/Documents (iCloud-synced; CLAUDE.md)" >&2; exit 2 ;; esac
mkdir -p "$(dirname "$OUT")"

W=$(mktemp -d /tmp/ocbm-overlay.XXXXXX)
trap 'rm -rf "$W"' EXIT
R=$W/root
mkdir -p "$R/bin" "$R/etc" "$R/script"

need() { [ -f "$1" ] || { echo "missing: $1 (build first: ~/.claude/bin/bt ./build.sh)" >&2; exit 1; }; }
put() { need "$1"; cp "$1" "$2"; chmod "$3" "$2"; }

put "$ARM/ocbmd" "$R/bin/ocbmd" 755
put "$OV/bin/ocbmd-launch" "$R/bin/ocbmd-launch" 755
put "$OV/etc/rc.ocbm" "$R/etc/rc.ocbm" 755
put "$OV/etc/hostapd.conf" "$R/etc/hostapd.conf" 644
put "$OV/etc/udhcpd.conf" "$R/etc/udhcpd.conf" 644
if [ "$PROFILE" = full ]; then
  put "$ARM/btd" "$R/bin/btd" 755
  put "$ARM/carplayd" "$R/bin/carplayd" 755
  put "$REPO/tools/session_supervisor.sh" "$R/script/session_supervisor.sh" 755
  put "$REPO/tools/projection_up.sh" "$R/script/projection_up.sh" 755
  for s in radio_detect.sh radio_hal.sh radio_ap_up.sh; do
    put "$REPO/ccpa/rootfs/script/$s" "$R/script/$s" 755
  done
fi
for s in "$R"/script/*.sh "$R/etc/rc.ocbm" "$R/bin/ocbmd-launch"; do
  [ -f "$s" ] || continue
  sh -n "$s" || { echo "syntax error: $s" >&2; exit 1; }
done
# a detached `sh -c` block whose own text matches its `pkill -f "[x]yz"` kills itself (2026-10-30)
python3 -I "$REPO/tools/pkill_selfmatch_check.py" "$R"/script/*.sh "$R/etc/rc.ocbm" || { echo "self-matching pkill in a packed script" >&2; exit 1; }
for b in "$R"/bin/ocbmd "$R"/bin/btd "$R"/bin/carplayd; do
  [ -f "$b" ] || continue
  file "$b" | grep -q 'ARM, EABI5' || { echo "not an armv7 binary: $b ($(file -b "$b"))" >&2; exit 1; }
done
(cd "$R" && find . -type f ! -name MANIFEST | sort | xargs shasum -a 256 | sed 's|  \./|  |') > "$R/etc/MANIFEST"
find "$R" -exec touch -h -t 202610300000 {} +

(cd "$R" && find . -mindepth 1 | sort | sed 's|^\./||') > "$W/list"
tar --format ustar --no-recursion --uid 0 --gid 0 --uname root --gname root --no-xattrs --no-mac-metadata \
  -cf "$W/payload.tar" -C "$R" -T "$W/list"
gzip -9 -n -c "$W/payload.tar" > "$W/payload.tgz"

GIT=$(git -C "$REPO" rev-parse --short HEAD 2>/dev/null || echo nogit)
[ -n "$(git -C "$REPO" status --porcelain 2>/dev/null | head -1)" ] && GIT="$GIT-dirty"
OMD5=$(md5 -q "$R/bin/ocbmd" 2>/dev/null || md5sum "$R/bin/ocbmd" | cut -c1-32)
VERSION="$PROFILE $STAMP $GIT ocbmd-md5=${OMD5:0:12}"
PBYTES=$(wc -c < "$W/payload.tgz" | tr -d ' ')
PSHA=$(shasum -a 256 "$W/payload.tgz" | cut -d' ' -f1)
HDR=$(printf 'OCBM-OVERLAY 1\npayload_bytes=%s\npayload_sha256=%s\nversion=%s\n' "$PBYTES" "$PSHA" "$VERSION")
HDR="$HDR"$'\n'
[ ${#HDR} -lt 4096 ] || { echo "header too long" >&2; exit 1; }
{ printf '%s' "$HDR"; head -c $((4096 - ${#HDR})) /dev/zero; cat "$W/payload.tgz"; } > "$OUT"

# self-check, the same rules ocbm-overlay applies on the unit
SZ=$(wc -c < "$OUT" | tr -d ' ')
[ "$SZ" -eq $((4096 + PBYTES)) ] || { echo "size mismatch" >&2; exit 1; }
[ "$SZ" -le "$REGION" ] || { echo "image $SZ B does not fit the region ($REGION B)" >&2; exit 1; }
[ "$(head -n 1 "$OUT")" = "OCBM-OVERLAY 1" ] || { echo "header check failed" >&2; exit 1; }
[ "$(tail -c +4097 "$OUT" | shasum -a 256 | cut -d' ' -f1)" = "$PSHA" ] || { echo "payload hash check failed" >&2; exit 1; }
tail -c +4097 "$OUT" | tar -tzf - | grep -qx 'bin/ocbmd-launch' || { echo "archive has no bin/ocbmd-launch" >&2; exit 1; }
ISHA=$(shasum -a 256 "$OUT" | cut -d' ' -f1)
echo "$ISHA  $(basename "$OUT")" > "$OUT.sha256"
echo "overlay: $OUT"
echo "  profile $PROFILE, $SZ B (payload $PBYTES B, unpacked $(du -sk "$R" | cut -f1) KiB), region $REGION B"
echo "  version: $VERSION"
echo "  image sha256 $ISHA"
echo "  contents:"; sed "s/^/    /" "$R/etc/MANIFEST" || true
echo "  on the unit: ocbm-overlay write /tmp/$(basename "$OUT") $ISHA"
