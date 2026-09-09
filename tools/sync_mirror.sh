#!/bin/bash
# Sync this checkout into the publication mirror, then commit and push it.
#
# The mirror (~/Downloads/github/ocbm) is its own git repo with a GitHub remote, and its root IS this
# repo's content — no wrapper directory. Two things go wrong if the sync is done by hand:
#
#   1. Deletions get missed. The corpus consolidation deleted 66 documents; without --delete the
#      mirror keeps serving the stale ones, which is the exact failure the consolidation existed to
#      end.
#   2b. `.claude/` carries local tool-permission state, including PATHS INTO CLAUDE SESSION
#      TRANSCRIPTS (project dir + session UUID). It is gitignored so it never reached GitHub, but
#      rsync was still copying it into the publication directory. Excluded and removed.
#
#   2a. SYMLINKED build output publishes as a DANGLING LINK. `host/gm_ccpa/apk` is a symlink into
#      ~/.cache (build artifacts must not live under iCloud-synced ~/Documents — see CLAUDE.md), and
#      `rsync -a` copies the LINK, absolute target and all. On GitHub, or on any other machine, it
#      resolves to nothing. Caught by a --dry-run 2026-09-08, which is also why the excludes below are
#      SLASHLESS: `apk/` matches a directory only and would have missed it. They are also PATH-
#      ANCHORED to host/gm_ccpa: a bare `evidence` exclude is global and silently stopped syncing
#      pi/evidence and pizero/evidence, which ARE deliberately published and cited by docs. rsync
#      protects excluded paths from --delete, so that strands them published-but-frozen rather than
#      failing loudly.
#
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
  --exclude='.claude/' \
  --exclude='.DS_Store' \
  --exclude='* [0-9].*' \
  --exclude='* [0-9]' \
  --exclude='target' --exclude='build/' \
  --exclude='/host/gm_ccpa/apk' --exclude='/host/gm_ccpa/evidence' --exclude='/host/gm_ccpa/logs' \
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
( cd "$DST" && rm -rf build scratchpad reference old .serena target .claude \
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

# Fail loudly rather than publish something tracked that should not be.
if git status --porcelain | awk '{print $NF}' | grep -qE '(^|/)(target|build|scratchpad|reference)(/|$)|^host/gm_ccpa/(apk|evidence|logs)(/|$)|\.DS_Store$'; then
    echo "FATAL: build output or temp is staged for publication" >&2
    git status --porcelain | grep -E '(^|/)(target|build|scratchpad)(/|$)|^host/gm_ccpa/(apk|evidence|logs)(/|$)|\.DS_Store$' >&2
    exit 1
fi

git add -A
git commit -q -m "$SUBJECT"
echo "committed $(git rev-parse --short HEAD): $SUBJECT"
echo "size: $(du -sh . | cut -f1) · tracked: $(git ls-files | wc -l | tr -d ' ') files"
git push origin main
