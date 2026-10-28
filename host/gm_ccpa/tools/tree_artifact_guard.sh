#!/bin/bash
# In-tree build-artifact guard, run first by tools/test.sh (owner rule, 2026-10-08).
#
# ~/Documents is iCloud "Desktop & Documents" synced on this Mac, and iCloud drops conflict copies
# (`classes 2.jar`, `Foo$Bar 2.dex`) into build trees, which toolchains then eat as real inputs — the
# nondeterministic duplicate-class failures ccpa_custom/CLAUDE.md describes. Every build system here is
# therefore pointed at ~/.cache (cargo via CARGO_TARGET_DIR, the APK work dir via mktemp, apk/ as a
# symlink), and this script makes the rule enforced rather than stated: a REAL directory with a
# build-output name inside the tree fails the gate and names itself. A symlink into ~/.cache is the
# sanctioned shape (ccpa_custom/target is one) and passes.
#
# Usage: tree_artifact_guard.sh <root>...   Exit 0 = clean; 1 = offenders printed (path and size).
# Scanned names: target build .gradle __pycache__ .build node_modules. Not descended: .git, and any
# `reference/` tree (documented read-only vendor material). Override ONLY for a run you know is dirty
# and are about to clean up: GMCCPA_ALLOW_TREE_ARTIFACTS=1 turns the failure into a WARNING.
set -u
[ $# -gt 0 ] || { echo "usage: $0 <root>..." >&2; exit 2; }
NAMES=(target build .gradle __pycache__ .build node_modules)
nameargs=()
for n in "${NAMES[@]}"; do nameargs+=(-o -name "$n"); done
found=()
while IFS= read -r d; do
    [ -n "$d" ] || continue
    [ -L "$d" ] && continue
    found+=("$d")
done < <(find "$@" \( -name .git -o -name reference \) -prune -o \
              -type d \( -false "${nameargs[@]}" \) -print -prune 2>/dev/null)
if [ "${#found[@]}" -eq 0 ]; then
    echo "[Tier-0] tree-artifact guard: clean (no real build-output directories under: $*)"
    exit 0
fi
echo "[Tier-0] tree-artifact guard: ${#found[@]} real build-output director$([ "${#found[@]}" -eq 1 ] && echo y || echo ies) INSIDE the tree:"
for d in "${found[@]}"; do
    printf '    %s\t%s\n' "$(du -sh "$d" 2>/dev/null | cut -f1)" "$d"
done
echo "  Build output must live under ~/.cache (ccpa_custom/CLAUDE.md: ~/Documents is iCloud-synced)."
echo "  Point the producing tool at CARGO_TARGET_DIR / a mktemp dir, then delete these (git-ignored:"
echo "  root .gitignore '**/target'; deletion needs no git action)."
if [ "${GMCCPA_ALLOW_TREE_ARTIFACTS:-0}" = "1" ]; then
    echo "  WARNING: GMCCPA_ALLOW_TREE_ARTIFACTS=1 — continuing; unset it for any gate run that counts."
    exit 0
fi
echo "[Tier-0] FAIL: in-tree build artifacts (set GMCCPA_ALLOW_TREE_ARTIFACTS=1 only to run the rest of the gate while cleaning up)."
exit 1
