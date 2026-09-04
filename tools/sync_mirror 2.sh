#!/bin/bash
# Sync this checkout into the publication mirror, then commit and push it.
#
# The mirror (~/Downloads/github/ocbm) is its own git repo with a GitHub remote, and its root IS this
# repo's content — no wrapper directory. Two things go wrong if the sync is done by hand:
#
#   1. Deletions get missed. The corpus consolidation deleted 66 documents; without --delete the
#      mirror keeps serving the stale ones, which is the exact failure the consolidation existed to
#      end.
#   2. Gitignored build output rides along. None of it is ever COMMITTED (the .gitignore travels with
#      the tree), but it accumulated 815 MB of Xcode DerivedData, cargo targets, .gradle caches and a
#      stale certs/ copy in a directory that syncs to GitHub. Excluded here, once, rather than
#      remembered each time.
#
# The mirror's own .git is never touched: it keeps its own history, and this script adds one commit
# per sync. Usage: tools/sync_mirror.sh ["commit subject"]
set -euo pipefail

SRC="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/"
DST="${OCBM_MIRROR:-$HOME/Downloads/github/ocbm}/"
SUBJECT="${1:-sync: mirror ccpa_custom @ $(git -C "$SRC" branch --show-current)}"

[ -d "$DST/.git" ] || { echo "FATAL: no git repo at $DST" >&2; exit 1; }

rsync -a --delete \
  --exclude='.git/' \
  --exclude='.DS_Store' \
  --exclude='target' --exclude='build/' \
  --exclude='.gradle/' --exclude='local.properties' \
  --exclude='xcuserdata/' \
  --exclude='.serena/' \
  --exclude='scratchpad/' --exclude='reference/' --exclude='old/' \
  --exclude='certs/' \
  --exclude='*.o' --exclude='*.armv7' --exclude='*.packed' \
  "$SRC" "$DST"

# rsync protects excluded paths from --delete, so anything that predates an exclusion survives.
# Remove those explicitly. All of it is gitignored, so nothing tracked can be lost here.
( cd "$DST" && find . -name '.DS_Store' -not -path './.git/*' -delete )
( cd "$DST" && rm -rf build scratchpad reference old .serena target \
    host/CarPlayHost/build host/CarlinkAndroid/.gradle host/CarlinkAndroid/local.properties \
    host/aa-headunit/certs \
    host/CarPlayHost/carlink_macOS.xcodeproj/xcuserdata \
    host/CarPlayHost/carlink_macOS.xcodeproj/project.xcworkspace/xcuserdata )

cd "$DST"
if [ -z "$(git status --porcelain)" ]; then
    echo "mirror already current at $(git rev-parse --short HEAD)"
    exit 0
fi

# Fail loudly rather than publish something tracked that should not be.
if git status --porcelain | awk '{print $NF}' | grep -qE '(^|/)(target|build|scratchpad|reference)/|\.DS_Store$'; then
    echo "FATAL: build output or temp is staged for publication" >&2
    git status --porcelain | grep -E '(^|/)(target|build|scratchpad)/|\.DS_Store$' >&2
    exit 1
fi

git add -A
git commit -q -m "$SUBJECT"
echo "committed $(git rev-parse --short HEAD): $SUBJECT"
echo "size: $(du -sh . | cut -f1) · tracked: $(git ls-files | wc -l | tr -d ' ') files"
git push origin main
