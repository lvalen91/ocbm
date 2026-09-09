#!/bin/sh
# Build OCBM v1 (Rust). Box daemon = armv7-musl via cargo-zigbuild (zig linker, no Linux VM);
# host client = native. Needs: rustup + `rustup target add armv7-unknown-linux-musleabihf`
# + `cargo install cargo-zigbuild`.
set -e
cd "$(dirname "$0")"
export PATH="$HOME/.cargo/bin:$PATH"

# BOX BINARIES COMPILE std FROM SOURCE (`-Z build-std`), so the size-first release profile
# (opt-level="z" + lto + panic=abort + codegen-units=1) applies to the standard library too,
# not just to our crates. Against a prebuilt std this project was paying a ~307 KB floor per
# binary; measured on ocbmd, build-std takes 512,960 -> 432,160 bytes (-15.8%) with no code
# change. That is the cheapest size lever available here: it moves no process boundary, changes
# no behaviour, and needs no on-box verification beyond "it still runs".
#
# Deliberately NOT `build-std-features=panic_immediate_abort`. That is worth roughly another
# 160 KB per binary, but it strips the panic message text — and on this box a panic message in
# /tmp/box.log is often the only diagnostic anyone gets, since there is no UART on most units and
# no debugger anywhere. Keep the messages; take the 15%.
#
# Requires nightly + the `rust-src` component (`rustup component add rust-src --toolchain
# nightly`). The riscv32 C2Air port has built this way since it existed
# (c2air/tools/c2air_ocbm_install.sh:161) — this brings the CCPA box target into line with it.
# The host binaries below (ocbm-host, mfi-probe) stay on stable: they are not size-constrained.
BOX_CARGO="rustup run nightly cargo"
BOX_STD="-Z build-std=std,panic_abort"

$BOX_CARGO zigbuild $BOX_STD --target armv7-unknown-linux-musleabihf --release -p ocbmd
echo "built  target/armv7-unknown-linux-musleabihf/release/ocbmd  ($(wc -c < target/armv7-unknown-linux-musleabihf/release/ocbmd) bytes, armv7 static)"

cargo build --release -p ocbm-host
echo "built  target/release/ocbm-host  ($(wc -c < target/release/ocbm-host) bytes)"

# iap2d (phone-side iAP2 accessory daemon) — needs the sibling carplay-iap2-core path dep;
# non-fatal so the core build still succeeds where that checkout is absent.
if $BOX_CARGO zigbuild $BOX_STD --target armv7-unknown-linux-musleabihf --release -p iap2d 2>/dev/null; then
  echo "built  target/armv7-unknown-linux-musleabihf/release/iap2d  ($(wc -c < target/armv7-unknown-linux-musleabihf/release/iap2d) bytes, armv7 static)"
else
  echo "skip   iap2d (carplay-iap2-core path dep unavailable — see HANDOFF Build)"
fi

# carplayd (box AirPlay/CarPlay daemon) — receiver's mic-uplink-eld feature statically links the
# cross-built libfdk-aac, so the eld-codec C shim needs the zig toolchain + FDK_AAC_PREFIX.
# Env per docs/ops/04_OPEN_ITEMS.md; skipped (non-fatal) where the fdk prefix is absent.
FDK="${FDK_AAC_PREFIX:-$PWD/scratchpad/fdk/install}"
if [ -d "$FDK" ]; then
  FDK_AAC_PREFIX="$FDK" \
  CC="zig cc -target arm-linux-musleabihf -mcpu=cortex_a7 -fno-sanitize=all" \
  AR="zig ar" \
  $BOX_CARGO zigbuild $BOX_STD --target armv7-unknown-linux-musleabihf --release -p carplayd
  echo "built  target/armv7-unknown-linux-musleabihf/release/carplayd  ($(wc -c < target/armv7-unknown-linux-musleabihf/release/carplayd) bytes, armv7 static)"
else
  echo "skip   carplayd (no fdk-aac prefix at $FDK — see docs/ops/04_OPEN_ITEMS.md)"
fi

# btd (BT bring-up/SSP/SDP/RFCOMM + WiFi handoff) — shipped box binary, plain cross build.
# NO rx-connect line: the mDNS advertiser was merged into carplayd on 2026-09-08
# (ccpa/carplayd/src/discovery.rs), which also removed tokio from the box set entirely.
$BOX_CARGO zigbuild $BOX_STD --target armv7-unknown-linux-musleabihf --release -p btd
echo "built  target/armv7-unknown-linux-musleabihf/release/btd  ($(wc -c < target/armv7-unknown-linux-musleabihf/release/btd) bytes, armv7 static)"

# aa-bridge (Android Auto AOAP pump, docs/androidauto/00_ARCHITECTURE.md) — shipped box binary: tools/ocbm_install.sh installs
# $ARM/aa-bridge to /usr/sbin/aa-bridge, so it has to be built here or the install line finds nothing.
$BOX_CARGO zigbuild $BOX_STD --target armv7-unknown-linux-musleabihf --release -p aa-bridge
# NOTE: no aa-wireless line. Wireless Android Auto is a LIBRARY linked into btd, not a
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
$BOX_CARGO zigbuild $BOX_STD --target armv7-unknown-linux-musleabihf --release -p mfid
echo "built  target/armv7-unknown-linux-musleabihf/release/mfid  ($(wc -c < target/armv7-unknown-linux-musleabihf/release/mfid) bytes, armv7 static, bring-up only)"
cargo build --release -p mfi-probe
echo "built  target/release/mfi-probe  ($(wc -c < target/release/mfi-probe) bytes, host)"
