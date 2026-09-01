#!/bin/bash
# Hardware-free CLI test run: compiles the LIVE OCBM sources together with the
# test harness and executes it. No Xcode project, no dongle, no code signing.
#
# After the legacy Carlinkit 0x55AA55AA Protocol/ layer was deleted, the harness
# exercises OCBM framing/reassembly, the OCBMClient subscribe state machine (via a
# FakeTransport), the pure AVCCFastPath helpers, and the Data LE helpers. The
# OCBMClient half compiles here because OCBMSessionCoordinator (the only IOKit /
# USBTransport-coupled piece) was split into its own file — OCBMClient talks to the
# pipe purely through the RawBulkTransport protocol.
set -euo pipefail
cd "$(dirname "$0")/.."

export DEVELOPER_DIR="${DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}"

# ── App→box YAML drift guard ────────────────────────────────────────────────────────────
# The Rust suite validates a LITERAL copy of the app's emitted document. If the emitter changes
# and nobody regenerates that literal, the Rust tests keep passing against a document the app no
# longer sends — green tests, unguarded wire. `regen_app_yaml_fixture.py` made regeneration
# executable; this makes it ENFORCED, which is the difference between a promise and a mechanism.
#
# Exit 2 (generator unavailable) is tolerated so the suite still runs where `xcrun swift` is not
# usable; a STALE fixture (exit 1) is fatal.
REPO_ROOT="$(cd ../.. && pwd)"
if [ -f "$REPO_ROOT/tools/check_app_yaml_fixture.py" ]; then
    set +e
    python3 "$REPO_ROOT/tools/check_app_yaml_fixture.py" "$REPO_ROOT"
    FIXTURE_RC=$?
    set -e
    [ "$FIXTURE_RC" -eq 1 ] && exit 1
fi

OUT="$(mktemp -d)/ocbm_tests"

# macos15 minimum: OCBMClient/OCBMAVDecrypt use Synchronization.Mutex (macOS 15+).
xcrun swiftc \
    -O \
    -target "$(uname -m)-apple-macos15" \
    carlink_macOS/App/InputTypes.swift \
    carlink_macOS/App/VehicleConfig.swift \
    carlink_macOS/OCBM/OCBMFraming.swift \
    carlink_macOS/OCBM/OCBMAVDecrypt.swift \
    carlink_macOS/OCBM/StreamMetrics.swift \
    carlink_macOS/OCBM/OCBMClient.swift \
    carlink_macOS/OCBM/OCBMControlRelay.swift \
    carlink_macOS/OCBM/AirPlaySetupSession.swift \
    carlink_macOS/Video/AVCCFastPath.swift \
    carlink_macOS/AA/AAWire.swift \
    tests/main.swift \
    -o "$OUT"

exec "$OUT"
