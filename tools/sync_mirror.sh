#!/bin/bash
# Sync the COMMITTED state of this repository (HEAD) into the publication mirror, then commit it there.
# Nothing is pushed unless --push is given.
#
# The mirror is its own git repo with a GitHub remote, outside this repo; its path comes from
# OCBM_MIRROR, which is REQUIRED (no default: exits 2 when unset). Its root IS this
# repo's content, no wrapper directory. It keeps its own history and gets one commit per sync, whose
# message names the source commit.
#
# Design (2026-10-28):
#   1. The source is `git archive HEAD`, not the working tree. Uncommitted edits, untracked local
#      state (.agents/, AGENTS.md, scratch files) and ignored build output therefore cannot reach the
#      mirror, and the mirror commit can name exactly what it contains. Commit first, then sync.
#   2. rsync --delete keeps the mirror free of stale files (the 66-document consolidation was not
#      reflected in the mirror until deletions were synced). rsync protects excluded paths from
#      --delete, so anything excluded that already sits in the mirror is removed explicitly below.
#   3. Three files are deliberately KEPT PUBLISHED although they are no longer tracked here
#      (owner decision 2026-10-28): the baseline rootfs images under boards/ccpa-imx6ul/stock/
#      (until 2026-11-03: ccpa/backup/base/). They embed device credentials, exist nowhere in this
#      repo and are already public in the mirror's history; they are protected from --delete by
#      PATH and are not added, changed or removed by this script.
#   4. The excludes are anchored where a bare name would also match tracked source (a bare
#      `reference/` dropped host/gm_ccpa/reference). Symlinked build output publishes as a dangling
#      link, so those are excluded slashless.
#   5. Before committing, the mirror's file list is compared with HEAD's tracked list; anything in the
#      mirror that HEAD does not track (other than the protected files) aborts the run.
#   6. ONE-TIME MIRROR RENAME (2026-11-03). The stock images moved from ccpa/backup/base/ to
#      boards/ccpa-imx6ul/stock/, and PROTECTED names the new paths only. A mirror that still holds
#      the three images at the old path would have them DELETED by rsync --delete, and this repo
#      cannot restore them. So before syncing (also under --dry-run) the run aborts with exit 2
#      while the mirror has any file under ccpa/backup/ (tracked, or on disk other than .DS_Store).
#      Do the rename by hand once, inside the mirror, and commit it:
#          cd "$OCBM_MIRROR"
#          mkdir -p boards/ccpa-imx6ul/stock
#          git ls-files ccpa/backup | while IFS= read -r f; do git mv "$f" boards/ccpa-imx6ul/stock/; done
#          git commit -m "Move the CCPA stock images from ccpa/backup/base/ to boards/ccpa-imx6ul/stock/"
#      That moves the three protected images plus the old copies of the six tracked files (README.md,
#      SHA256SUMS, manifest.txt, rootfs.files.txt, mtd0.bin, mtd1.bin), which the next sync then
#      overwrites from HEAD. Then check with --dry-run that no "*deleting" line names a protected file.
#
# Usage: tools/sync_mirror.sh [--dry-run] [--push] ["commit subject"]
#   --dry-run  list what rsync would create, update and delete; change nothing
#   --push     after committing, push origin main (the mirror's pre-push hook refuses session references)
set -euo pipefail

DRY=0; PUSH=0; SUBJECT=""
for a in "$@"; do
    case "$a" in
        --dry-run) DRY=1 ;;
        --push)    PUSH=1 ;;
        -*)        echo "FATAL: unknown option $a" >&2; exit 2 ;;
        *)         SUBJECT="$a" ;;
    esac
done

SRC="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
[ -n "${OCBM_MIRROR:-}" ] || { echo "FATAL: set OCBM_MIRROR to the publication mirror checkout" >&2; exit 2; }
DST="$OCBM_MIRROR"
SRC_REV="$(git -C "$SRC" rev-parse HEAD)"
SUBJECT="${SUBJECT:-sync: mirror ocbm @ ${SRC_REV:0:7}}"

[ -d "$DST/.git" ] || { echo "FATAL: no git repo at $DST" >&2; exit 1; }

# Kept published although untracked here (see design note 3).
PROTECTED=(
  boards/ccpa-imx6ul/stock/rootfs.tar.gz
  boards/ccpa-imx6ul/stock/mtd2.bin
  boards/ccpa-imx6ul/stock/CPC200-CCPA_full_nor_16MB.bin
)

# The old location of the protected files must be empty before anything syncs (see design note 6):
# PROTECTED no longer covers it, so rsync --delete would remove whatever still sits there.
OLD_STOCK=ccpa/backup
OLD_LEFT="$( { git -C "$DST" ls-files -- "$OLD_STOCK"
               if [ -d "$DST/$OLD_STOCK" ]; then ( cd "$DST" && find "$OLD_STOCK" -type f ! -name '.DS_Store' ); fi
             } | sort -u )"
if [ -n "$OLD_LEFT" ]; then
    echo "FATAL: the mirror still holds files under $OLD_STOCK/ (the pre-2026-11-03 stock-image folder):" >&2
    printf '%s\n' "$OLD_LEFT" | head -20 >&2
    echo "PROTECTED now names boards/ccpa-imx6ul/stock/, so this sync would DELETE them. Do the one-time" >&2
    echo "rename inside the mirror first and commit it (tools/sync_mirror.sh design note 6):" >&2
    echo "  cd \"\$OCBM_MIRROR\" && mkdir -p boards/ccpa-imx6ul/stock" >&2
    echo "  git ls-files $OLD_STOCK | while IFS= read -r f; do git mv \"\$f\" boards/ccpa-imx6ul/stock/; done" >&2
    echo "  git commit -m \"Move the CCPA stock images from ccpa/backup/base/ to boards/ccpa-imx6ul/stock/\"" >&2
    echo "Untracked files left under $OLD_STOCK/ must be moved or removed by hand." >&2
    exit 2
fi

if [ "$DRY" = 0 ]; then
    # The licence files must be part of the published commit.
    for f in LICENSE NOTICE.md baseline/LICENSE; do
        git -C "$SRC" cat-file -e "HEAD:$f" 2>/dev/null \
            || { echo "FATAL: $f is not committed in the source" >&2; exit 1; }
    done
    # The mirror must not carry hand edits or a half-finished earlier sync.
    if [ -n "$(git -C "$DST" status --porcelain)" ]; then
        echo "FATAL: mirror working tree is dirty; resolve it first:" >&2
        git -C "$DST" status --short | head -20 >&2
        exit 1
    fi
fi

STAGE="$(mktemp -d "${TMPDIR:-/tmp}/ocbm-sync.XXXXXX")"
trap 'rm -rf "$STAGE"' EXIT
git -C "$SRC" archive HEAD | tar -x -C "$STAGE"

RSYNC_OPTS=(-a --delete
  --exclude='.git/'
  --exclude='.claude/' --exclude='/.agents/' --exclude='/AGENTS.md' --exclude='/inlist.swift'
  --exclude='.DS_Store'
  --exclude='* [0-9].*' --exclude='* [0-9]'
  --exclude='target' --exclude='build/'
  --exclude='/host/gm_ccpa/apk' --exclude='/host/gm_ccpa/evidence' --exclude='/host/gm_ccpa/logs'
  --exclude='.gradle/' --exclude='.kotlin/' --exclude='local.properties'
  --exclude='xcuserdata/' --exclude='*.xcuserstate' --exclude='DerivedData/' --exclude='node_modules/'
  --exclude='.serena/'
  --exclude='/scratchpad/' --exclude='/reference/' --exclude='/old/'
  --exclude='certs/'
  --exclude='*.o' --exclude='*.armv7' --exclude='*.packed'
  --exclude='*.apk' --exclude='*.dex')
for p in "${PROTECTED[@]}"; do RSYNC_OPTS+=("--filter=P /$p"); done

if [ "$DRY" = 1 ]; then
    rsync "${RSYNC_OPTS[@]}" -n -i "$STAGE/" "$DST/"
    exit 0
fi

rsync "${RSYNC_OPTS[@]}" "$STAGE/" "$DST/"

# Remove what predates an exclusion (all gitignored, so nothing tracked can be lost here).
( cd "$DST" && find . -name '.DS_Store' -not -path './.git/*' -delete )
( cd "$DST" && rm -rf build scratchpad reference old .serena target .claude .agents AGENTS.md inlist.swift \
    host/gm_ccpa/apk host/gm_ccpa/evidence host/gm_ccpa/logs \
    host/MacHost/build host/CarlinkAndroid/.gradle host/CarlinkAndroid/local.properties \
    host/aa-headunit/certs \
    host/MacHost/carlink_macOS.xcodeproj/xcuserdata \
    host/MacHost/carlink_macOS.xcodeproj/project.xcworkspace/xcuserdata )

cd "$DST"
if [ -z "$(git status --porcelain)" ]; then
    echo "mirror already current at $(git rev-parse --short HEAD)"
    exit 0
fi

git add -A

# Fail loudly rather than publish something HEAD does not track. The protected files are expected.
EXPECTED="$(mktemp)"; ACTUAL="$(mktemp)"
git -C "$SRC" ls-tree -r --name-only HEAD | sort > "$EXPECTED"
git ls-files | sort > "$ACTUAL"
UNTRACKED_IN_SRC="$(comm -13 "$EXPECTED" "$ACTUAL")"
for p in "${PROTECTED[@]}"; do
    UNTRACKED_IN_SRC="$(printf '%s\n' "$UNTRACKED_IN_SRC" | grep -vxF "$p" || true)"
done
rm -f "$EXPECTED" "$ACTUAL"
if [ -n "$UNTRACKED_IN_SRC" ]; then
    echo "FATAL: the mirror holds files the source does not track:" >&2
    printf '%s\n' "$UNTRACKED_IN_SRC" | head -30 >&2
    git reset -q    # unstage only; leaves the mirror tree for inspection
    exit 1
fi

git commit -q -m "$SUBJECT" -m "Source: ocbm @ $SRC_REV"
echo "committed $(git rev-parse --short HEAD): $SUBJECT"
echo "size: $(du -sh . | cut -f1) · tracked: $(git ls-files | wc -l | tr -d ' ') files"
if [ "$PUSH" = 1 ]; then
    git push origin main
else
    echo "not pushed (re-run with --push, or: git -C $DST push origin main)"
fi
