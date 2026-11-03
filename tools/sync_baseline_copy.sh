#!/usr/bin/env bash
# sync_baseline_copy.sh -- keep the CPC200-CCPA_resources copy of baseline/ identical to this tree.
# Default is a dry run (prints what rsync would change). Pass --apply to write to the copy.
# The copy is a path-for-path mirror: custom/kernel_rootfs/baseline in the resources repo.
set -euo pipefail
SRC="$(cd "$(dirname "$0")/.." && pwd)/baseline/"
# RESOURCES_BASELINE: custom/kernel_rootfs/baseline inside a CPC200-CCPA_resources checkout (outside the repo; required).
[ -n "${RESOURCES_BASELINE:-}" ] || { echo "set RESOURCES_BASELINE to <CPC200-CCPA_resources>/custom/kernel_rootfs/baseline" >&2; exit 2; }
DST="${RESOURCES_BASELINE%/}/"
[ -d "$DST" ] || { echo "no such directory: $DST" >&2; exit 2; }
flags=(-a --delete --exclude .DS_Store --exclude .git)
if [ "${1:-}" = "--apply" ]; then rsync "${flags[@]}" "$SRC" "$DST"; else rsync -n -i "${flags[@]}" "$SRC" "$DST"; fi
diff -rq -x .DS_Store "$SRC" "$DST" && echo "baseline copy identical" || true
