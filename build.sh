#!/bin/sh
# Build OCBM v1 (Rust). Box daemon = armv7-musl via cargo-zigbuild (zig linker, no Linux VM);
# host client = native. Needs: rustup + `rustup target add armv7-unknown-linux-musleabihf`
# + `cargo install cargo-zigbuild`.
set -e
cd "$(dirname "$0")"
export PATH="$HOME/.cargo/bin:$PATH"

cargo zigbuild --target armv7-unknown-linux-musleabihf --release -p ocbmd
echo "built  target/armv7-unknown-linux-musleabihf/release/ocbmd  ($(wc -c < target/armv7-unknown-linux-musleabihf/release/ocbmd) bytes, armv7 static)"

cargo build --release -p ocbm-host
echo "built  target/release/ocbm-host  ($(wc -c < target/release/ocbm-host) bytes)"

# iap2d (phone-side iAP2 accessory daemon) — needs the sibling carplay-iap2-core path dep;
# non-fatal so the core build still succeeds where that checkout is absent.
if cargo zigbuild --target armv7-unknown-linux-musleabihf --release -p iap2d 2>/dev/null; then
  echo "built  target/armv7-unknown-linux-musleabihf/release/iap2d  ($(wc -c < target/armv7-unknown-linux-musleabihf/release/iap2d) bytes, armv7 static)"
else
  echo "skip   iap2d (carplay-iap2-core path dep unavailable — see HANDOFF Build)"
fi

# airplayd (box AirPlay/CarPlay daemon) — receiver's mic-uplink-eld feature statically links the
# cross-built libfdk-aac, so the eld-codec C shim needs the zig toolchain + FDK_AAC_PREFIX.
# Env per docs/ops/04_OPEN_ITEMS.md; skipped (non-fatal) where the fdk prefix is absent.
FDK="${FDK_AAC_PREFIX:-$PWD/scratchpad/fdk/install}"
if [ -d "$FDK" ]; then
  FDK_AAC_PREFIX="$FDK" \
  CC="zig cc -target arm-linux-musleabihf -mcpu=cortex_a7 -fno-sanitize=all" \
  AR="zig ar" \
  cargo zigbuild --target armv7-unknown-linux-musleabihf --release -p airplayd
  echo "built  target/armv7-unknown-linux-musleabihf/release/airplayd  ($(wc -c < target/armv7-unknown-linux-musleabihf/release/airplayd) bytes, armv7 static)"
else
  echo "skip   airplayd (no fdk-aac prefix at $FDK — see docs/ops/04_OPEN_ITEMS.md)"
fi

# carplay-wireless (BT bring-up/SSP/SDP/RFCOMM + WiFi handoff) + rx-connect (mDNS advertiser) —
# shipped box binaries, plain cross builds.
cargo zigbuild --target armv7-unknown-linux-musleabihf --release -p carplay-wireless
echo "built  target/armv7-unknown-linux-musleabihf/release/carplay-wireless  ($(wc -c < target/armv7-unknown-linux-musleabihf/release/carplay-wireless) bytes, armv7 static)"
cargo zigbuild --target armv7-unknown-linux-musleabihf --release -p rx-connect
echo "built  target/armv7-unknown-linux-musleabihf/release/rx-connect  ($(wc -c < target/armv7-unknown-linux-musleabihf/release/rx-connect) bytes, armv7 static)"

# aa-bridge (Android Auto AOAP pump, docs/androidauto/00_ARCHITECTURE.md) — shipped box binary: tools/ocbm_install.sh installs
# $ARM/aa-bridge to /usr/sbin/aa-bridge, so it has to be built here or the install line finds nothing.
cargo zigbuild --target armv7-unknown-linux-musleabihf --release -p aa-bridge
# NOTE: no aa-wireless line. Wireless Android Auto is a LIBRARY linked into carplay-wireless, not a
# daemon of its own — see ccpa/aa-wireless/src/lib.rs. It builds as part of that binary.
echo "built  target/armv7-unknown-linux-musleabihf/release/aa-bridge  ($(wc -c < target/armv7-unknown-linux-musleabihf/release/aa-bridge) bytes, armv7 static)"

# iap_role_switch (phone-side 0x51 USB host-role switch helper) — static armv7 C, built with zig cc.
if command -v zig >/dev/null 2>&1; then
  zig cc -target arm-linux-musleabihf -static -Os -s -o accessory_init/iap_role_switch.armv7 accessory_init/iap_role_switch.c
  echo "built  accessory_init/iap_role_switch.armv7  ($(wc -c < accessory_init/iap_role_switch.armv7) bytes, armv7 static)"
else
  echo "skip   iap_role_switch (zig not found)"
fi

# mfid + mfi-probe — NCM bring-up instruments, NOT shipped. mfid serves the two MFi chip ops over
# TCP so the Pi can reach the coprocessor while OCBM is not the transport; mfi-probe exercises it
# from the host. Deployed only via tools/run_mfid.sh, which stages to /tmp and cleans up after.
cargo zigbuild --target armv7-unknown-linux-musleabihf --release -p mfid
echo "built  target/armv7-unknown-linux-musleabihf/release/mfid  ($(wc -c < target/armv7-unknown-linux-musleabihf/release/mfid) bytes, armv7 static, bring-up only)"
cargo build --release -p mfi-probe
echo "built  target/release/mfi-probe  ($(wc -c < target/release/mfi-probe) bytes, host)"
