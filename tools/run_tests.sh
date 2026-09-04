#!/bin/sh
# run_tests.sh — the ACTUAL test suite.
#
# `cargo test` and `cargo test --workspace` at the repo root do NOT run most of this project's tests:
# `crates/vendor/{iap2-core,receiver,rtsp,pairing,mfi,mfi-i2c-local,metadata,eld-codec}` are all in
# the root Cargo.toml `exclude` list, so they build as path dependencies only. The root workspace also
# fails to compile on macOS (`airplayd` uses `libc::TCP_KEEPIDLE`, Linux-only). Both facts together
# meant ~130 tests could be added, or silently broken, without anything noticing.
#
# `receiver` is run with --no-default-features: the default set pulls `eld-codec`, whose build script
# needs a cross-built libfdk-aac that is not present on a plain host checkout.
set -e
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"

fail=0
run() {
  echo "=== $1"
  shift
  "$@" || fail=1
}

run "iap2-core"  cargo test -p carplay-iap2-core --quiet
run "receiver"   cargo test --manifest-path crates/vendor/receiver/Cargo.toml --no-default-features --quiet
# ...and again in the BOX configuration. `local-mfi` (direct /dev/i2c-1 chip access) became an
# OPTIONAL feature on 2026-08-27 so the Android head-unit app can leave it off and install a remote
# signer instead. That split means the two configurations compile DIFFERENT code, and the line above
# only ever exercises the Android one -- a `local-mfi`-only break would ship to the box unseen.
run "receiver (box: local-mfi)" cargo test --manifest-path crates/vendor/receiver/Cargo.toml \
  --no-default-features --features local-mfi --quiet
run "ocbm-proto" cargo test -p ocbm-proto --quiet
run "ocbmd"      cargo test -p ocbmd --quiet
run "iap2d"      cargo test -p iap2d --quiet
# These are excluded from the workspace too, and were missing from this script until 2026-07-29 —
# 103 further passing tests, including `carplay-wireless`, which is one of the four SHIPPED box
# binaries, and `carplay-metadata`, a direct dependency of iap2-core.
run "metadata"   cargo test --manifest-path crates/vendor/metadata/Cargo.toml --quiet
run "wireless"   cargo test -p carplay-wireless --quiet
# bt-common holds the Bluetooth primitives extracted from carplay-wireless on 2026-09-01. Its
# 29 tests used to run under the "wireless" line above; without this one they would silently
# stop running, which is the failure mode an extraction is most likely to cause.
run "bt-common"  cargo test -p bt-common --quiet
run "aa-wireless" cargo test -p aa-wireless --quiet
# aa-bridge became host-testable on 2026-09-04: its pure parts (the transport-agnostic copy loop and
# the app-socket broker that arbitrates :5277 between the wired and wireless arms) moved into
# `pump.rs`/`appport.rs`, and `box_common::usb`'s off-Linux stub was widened so the crate type-checks
# here. Registered immediately, for the reason the bt-common line above records.
run "aa-bridge"  cargo test -p aa-bridge --quiet
# box-common carries the shared flags/config/phone/AP-address definitions all of the above depend on.
run "box-common" cargo test -p box-common --quiet
run "pairing"    cargo test --manifest-path crates/vendor/pairing/Cargo.toml --quiet
run "rtsp"       cargo test --manifest-path crates/vendor/rtsp/Cargo.toml --quiet
run "mfi"        cargo test --manifest-path crates/vendor/mfi/Cargo.toml --quiet
run "ocbm-host"  cargo test -p ocbm-host --quiet

# host/CarPlayHost's own hardware-free Swift harness (tests/run_tests.sh) — no Xcode project, no
# dongle. Skipped (not a failure) when the Swift toolchain isn't on this machine, since the rest of
# this script is Rust-only and must still be runnable without Xcode installed.
if command -v xcrun >/dev/null 2>&1 && xcrun --find swiftc >/dev/null 2>&1; then
  run "host/CarPlayHost (Swift)" bash host/CarPlayHost/tests/run_tests.sh
else
  echo "=== host/CarPlayHost (Swift) — SKIPPED (xcrun/swiftc not found)"
fi

[ "$fail" -eq 0 ] || { echo "FAILURES"; exit 1; }
echo "all suites passed"
