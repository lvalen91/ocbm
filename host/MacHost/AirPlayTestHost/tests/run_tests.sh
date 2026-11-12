#!/bin/bash
# Hardware-free test run for the AirPlay Test Host: compiles its Core / Media / Session sources and the
# debug-only synthetic box with the shared carlink_macOS files they use, plus tests/main.swift, and runs
# the result. No Xcode project, no USB, no box, no signing. The media tests need ffmpeg (they are
# skipped, loudly, without it) and a VideoToolbox H.264 decoder; nothing is played on the output device.
#
#   tests/run_tests.sh            everything
#   KEEP_MEDIA=1 tests/run_tests.sh   keep the generated test media (path is printed)
set -euo pipefail
cd "$(dirname "$0")/.."

export DEVELOPER_DIR="${DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}"
SHARED=../carlink_macOS
WORK="$(mktemp -d /tmp/airplay-test-host-tests.XXXXXX)"
OUT="$WORK/airplay_test_host_tests"

# The shared files are the shipping app's, compiled as they are (see README.md for why each is here).
xcrun swiftc \
    -O -D DEBUG -swift-version 6 \
    -target "$(uname -m)-apple-macos27.0" \
    "$SHARED/App/InputTypes.swift" \
    "$SHARED/App/VehicleConfig.swift" \
    "$SHARED/App/Settings/VehicleProfile.swift" \
    "$SHARED/OCBM/OCBMFraming.swift" \
    "$SHARED/OCBM/OCBMAVDecrypt.swift" \
    "$SHARED/OCBM/ConfigIntegrity.swift" \
    "$SHARED/OCBM/SessionFailureTracker.swift" \
    "$SHARED/OCBM/StreamMetrics.swift" \
    "$SHARED/OCBM/OCBMClient.swift" \
    "$SHARED/Video/AVCCFastPath.swift" \
    "$SHARED/Audio/AudioPlayer.swift" \
    Sources/Core/*.swift \
    Sources/Media/*.swift \
    Sources/Session/*.swift \
    Sources/Debug/*.swift \
    tests/main.swift \
    -o "$OUT" 2> "$WORK/build.log" || { cat "$WORK/build.log" >&2; exit 1; }

rc=0
AIRPLAY_TEST_MEDIA_DIR="$WORK/media" "$OUT" || rc=$?
if [ "${KEEP_MEDIA:-0}" = 1 ]; then echo "test media kept in $WORK/media"; else rm -rf "$WORK"; fi
exit $rc
