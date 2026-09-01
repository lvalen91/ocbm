#!/usr/bin/env bash
# Cross-build libfdk-aac for aarch64-linux-android, so airplayd can be built WITH `mic-uplink-eld`.
#
#   pi/tools/build_fdk_aac_arm64.sh
#
# WHY THIS MATTERS MORE THAN IT LOOKS
#
# Without it, airplayd is built --no-default-features and reports:
#
#     [uplink] ... negotiated but `mic-uplink-eld` not built
#
# iOS negotiates the AAC-ELD mic uplink and receives SILENCE. Siri hears nothing and a phone call is
# one-way. Everything else about the voice path works — the downlink plays, ducking fires, the logs
# look healthy — so this reads as a working feature and is not.
#
# WHAT IT PRODUCES
#
#   scratchpad/fdk/install-android-arm64/{include/fdk-aac,lib/libfdk-aac.a}
#   scratchpad/fdk/stub/log/log.h
#
# `scratchpad/` is gitignored (a 10 MB archive does not belong in the repo), which is why this script
# exists: it is the reproducible form of those artefacts.

set -euo pipefail

here="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$here"
say() { printf '\033[1m==>\033[0m %s\n' "$*"; }

VERSION=2.0.3
FDK_DIR="$here/scratchpad/fdk"
SRC_TARBALL="$FDK_DIR/fdk-aac-$VERSION.tar.gz"
PREFIX="$FDK_DIR/install-android-arm64"
STUB="$FDK_DIR/stub"
API=34

# Build OUT of ~/Documents. That tree is covered by iCloud "Desktop & Documents" sync, which
# resolves its own races by writing conflict copies next to the original — the vendored source
# already contains a `config 2.log` from a previous build. autotools picking up a "* 2.*" file, or
# make consuming a duplicated object, produces failures that come and go. Only the finished install
# goes back into the repo tree.
BUILD_ROOT="${TMPDIR:-/tmp}/fdk-aac-arm64-build"

# ---- toolchain ---------------------------------------------------------------------------------

NDK_ROOT="${ANDROID_NDK_HOME:-}"
if [ -z "$NDK_ROOT" ]; then
    sdk="${ANDROID_HOME:-$HOME/Library/Android/sdk}"
    NDK_ROOT=$(ls -d "$sdk"/ndk/* 2>/dev/null | sort -V | tail -1 || true)
fi
[ -n "$NDK_ROOT" ] || { echo "no NDK found; set ANDROID_NDK_HOME" >&2; exit 1; }
PREBUILT=$(ls -d "$NDK_ROOT"/toolchains/llvm/prebuilt/* 2>/dev/null | head -1)
[ -x "$PREBUILT/bin/aarch64-linux-android$API-clang" ] || {
    echo "missing $PREBUILT/bin/aarch64-linux-android$API-clang" >&2; exit 1; }

say "NDK: $NDK_ROOT (API $API)"

# ---- the log/log.h stub -------------------------------------------------------------------------
#
# fdk-aac 2.0.3's libSBRdec/src/lpp_tran.cpp does `#ifdef __ANDROID__ / #include "log/log.h"`.
# That header is AOSP-INTERNAL and is not in the NDK (which ships <android/log.h> instead), so every
# NDK cross-build of fdk-aac fails on that line.
#
# Exactly one symbol is needed: android_errorWriteLog(), called twice. Both calls are the upstream
# fix for CVE-2018-9491 (AOSP bug 112160868) and sit in the ELSE branch of a bounds check:
#
#     if (<in-range>) { ...clear the buffer... }
#     #ifdef __ANDROID__
#     else { android_errorWriteLog(0x534e4554, "112160868"); }
#     #endif
#
# THE BOUNDS CHECK IS OUTSIDE THE IFDEF AND STILL RUNS. What the stub drops is only the REPORTING of
# a trip to Android's security event log — a platform facility a standalone static library cannot
# write to anyway. No security behaviour is removed; only telemetry that could not work here.
#
# A no-op inline, not an empty header: an empty header compiles and then fails at LINK time with an
# undefined symbol, which is a far worse place to discover this.
say "writing the log/log.h stub"
mkdir -p "$STUB/log"
cat > "$STUB/log/log.h" <<'STUB_EOF'
/* Stub for AOSP's internal <log/log.h> — see pi/tools/build_fdk_aac_arm64.sh for the full rationale.
 *
 * Needed only to cross-compile fdk-aac with the NDK. Supplies the single symbol lpp_tran.cpp uses,
 * android_errorWriteLog(), which upstream calls purely to report a CVE-2018-9491 bounds-check trip
 * to Android's security event log. The bounds check itself is outside the #ifdef and is unaffected.
 */
#ifndef FDK_AAC_STUB_LOG_LOG_H
#define FDK_AAC_STUB_LOG_LOG_H
#ifdef __cplusplus
extern "C" {
#endif
static inline int android_errorWriteLog(int tag, const char *subTag) {
    (void)tag;
    (void)subTag;
    return 0;
}
#ifdef __cplusplus
}
#endif
#endif
STUB_EOF

# ---- source --------------------------------------------------------------------------------------

rm -rf "$BUILD_ROOT"
mkdir -p "$BUILD_ROOT/src" "$BUILD_ROOT/build"

if [ -d "$FDK_DIR/fdk-aac-$VERSION" ]; then
    say "copying the vendored source out of the synced tree"
    # Drop iCloud conflict copies so autotools cannot pick one up.
    rsync -a --exclude='* 2.*' --exclude='*.o' --exclude='*.lo' --exclude='*.la' \
        "$FDK_DIR/fdk-aac-$VERSION/" "$BUILD_ROOT/src/"
elif [ -f "$SRC_TARBALL" ]; then
    say "extracting $SRC_TARBALL"
    tar -xzf "$SRC_TARBALL" -C "$BUILD_ROOT"
    mv "$BUILD_ROOT/fdk-aac-$VERSION"/* "$BUILD_ROOT/src/"
else
    echo "no fdk-aac source at $FDK_DIR/fdk-aac-$VERSION or $SRC_TARBALL" >&2
    exit 1
fi
# The vendored tree is usually already configured for another target; an out-of-tree configure
# refuses with "source directory already configured".
( cd "$BUILD_ROOT/src" && make distclean >/dev/null 2>&1 || true )

# ---- build ----------------------------------------------------------------------------------------

cd "$BUILD_ROOT/build"
export CC="$PREBUILT/bin/aarch64-linux-android$API-clang"
export CXX="$PREBUILT/bin/aarch64-linux-android$API-clang++"
export AR="$PREBUILT/bin/llvm-ar"
export RANLIB="$PREBUILT/bin/llvm-ranlib"
export CPPFLAGS="-I$STUB"

say "configure"
"$BUILD_ROOT/src/configure" --host=aarch64-linux-android --prefix="$PREFIX" \
    --enable-static --disable-shared --disable-dependency-tracking > configure.log 2>&1 || {
        tail -20 configure.log; exit 1; }

say "make"
make -j"$(sysctl -n hw.ncpu 2>/dev/null || nproc)" > build.log 2>&1 || { tail -20 build.log; exit 1; }

say "install -> $PREFIX"
make install > install.log 2>&1 || { tail -20 install.log; exit 1; }

# Guard against the classic cross-build failure: a host-arch archive that links on the dev machine
# and is rejected on the device.
if ! "$PREBUILT/bin/llvm-nm" --archive-headers "$PREFIX/lib/libfdk-aac.a" >/dev/null 2>&1; then
    echo "built archive is not readable by the NDK toolchain — wrong arch?" >&2
    exit 1
fi

ls -l "$PREFIX/lib/libfdk-aac.a"
cat <<EOF

Done. Build airplayd WITH mic uplink by exporting:

  export FDK_AAC_PREFIX=$PREFIX
  export CPPFLAGS=-I$STUB

pi/tools/build_pi_binaries.sh picks both up automatically when the prefix exists.

Verify the feature landed (this string must be ABSENT):
  strings target/aarch64-linux-android/release/airplayd | grep 'mic-uplink-eld\` not built'
EOF
