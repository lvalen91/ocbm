#!/usr/bin/env bash
# Cross-build the accessory-stack binaries for the Raspberry Pi (AAOS, aarch64) and stage them.
#
#   pi/tools/build_pi_binaries.sh [--serial <adb-serial>] [--no-push]
#
# WHY THE TARGET IS aarch64-linux-android AND NOT aarch64-unknown-linux-gnu
#
# The Pi runs Android. `target_os` is "android", not "linux", and that distinction has already cost
# this project real debugging time: cloexec.rs was gated on `cfg(target_os = "linux")`, silently
# fell through to the macOS branch on this target, and leaked every Bluetooth socket into the
# detached daemons — which then held L2CAP PSM 3 and made the next start fail with "Address already
# in use". See pi/docs/00 §4. Building for the gnu triple would reintroduce that whole class.
#
# HOW THE PUSH AVOIDS DISTURBING A LIVE SESSION
#
# You cannot write to a running executable — the kernel returns ETXTBSY. But you CAN rename one:
# the running process holds its inode and carries on. So this pushes to a staging name, renames the
# old binary aside, and moves the new one into place. Nothing running is touched; the next start
# picks up the new code. Restart with pi/tools/start_stack.sh --restart when you are ready.

set -euo pipefail

SERIAL=""
PUSH=1
while [ $# -gt 0 ]; do
    case "$1" in
        --serial)  SERIAL="$2"; shift ;;
        --no-push) PUSH=0 ;;
        -h|--help) sed -n '2,24p' "$0"; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
    shift
done

here="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$here"
adb=(adb)
[ -n "$SERIAL" ] && adb=(adb -s "$SERIAL")
say() { printf '\033[1m==>\033[0m %s\n' "$*"; }

TARGET=aarch64-linux-android
API=34
TMP=/data/local/tmp

# ---- toolchain ---------------------------------------------------------------------------------

# rustup's cargo, not Homebrew's: only the rustup toolchain has the Android target installed, and
# Homebrew cargo fails with a misleading "can't find crate for `core` ... target may not be
# installed" even though `rustup target list` shows it present.
export PATH="$HOME/.cargo/bin:$PATH"

NDK_ROOT="${ANDROID_NDK_HOME:-}"
if [ -z "$NDK_ROOT" ]; then
    sdk="${ANDROID_HOME:-$HOME/Library/Android/sdk}"
    NDK_ROOT=$(ls -d "$sdk"/ndk/* 2>/dev/null | sort -V | tail -1 || true)
fi
[ -n "$NDK_ROOT" ] || { echo "no NDK found; set ANDROID_NDK_HOME" >&2; exit 1; }

# Still named darwin-x86_64 on Apple Silicon (it runs under Rosetta or is a universal binary).
PREBUILT=$(ls -d "$NDK_ROOT"/toolchains/llvm/prebuilt/* 2>/dev/null | head -1)
CLANG="$PREBUILT/bin/aarch64-linux-android$API-clang"
[ -x "$CLANG" ] || { echo "missing $CLANG" >&2; exit 1; }

export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$CLANG"
export CC_aarch64_linux_android="$CLANG"
export AR_aarch64_linux_android="$PREBUILT/bin/llvm-ar"

say "NDK: $NDK_ROOT (API $API)"

# ---- build -------------------------------------------------------------------------------------

say "building carplay-wireless"
cargo build --release --target "$TARGET" -p carplay-wireless

# mic-uplink-eld needs a cross-built libfdk-aac. WITHOUT it iOS negotiates the AAC-ELD mic uplink
# and gets SILENCE — Siri hears nothing and a call is one-way — while every log line still looks
# healthy. So build it in whenever the library is available, and say plainly when it is not.
FDK_PREFIX="${FDK_AAC_PREFIX:-$here/scratchpad/fdk/install-android-arm64}"
FDK_STUB="$here/scratchpad/fdk/stub"
if [ -f "$FDK_PREFIX/lib/libfdk-aac.a" ]; then
    export FDK_AAC_PREFIX="$FDK_PREFIX"
    export CPPFLAGS="-I$FDK_STUB"
    # RUN THE ENCODER'S OWN TESTS FIRST — this is the only build path where they CAN run.
    #
    # `eld-codec` is behind the `mic-uplink-eld` feature and needs fdk-aac present, so it is invisible
    # to `cargo test` everywhere else. Its test `eld_16k_mono_asc_matches_iphone` had been FAILING for
    # the entire life of the mic uplink, asserting the exact 4-byte ASC that iOS expects while the
    # encoder emitted a 7-byte LD-SBR one — and nothing ran it, so Siri heard silence and no suite
    # ever went red. A test that cannot run is not a test.
    #
    # They run for the HOST here, not the target: the assertion is about what fdk-aac's configuration
    # produces, which is architecture-independent, and a host run needs no device. FDK_AAC_PREFIX is
    # the arm64 build, so point the host run at whatever fdk-aac the host has.
    if [ -d /opt/homebrew/opt/fdk-aac ] || [ -d /usr/local/opt/fdk-aac ]; then
        host_fdk=$([ -d /opt/homebrew/opt/fdk-aac ] && echo /opt/homebrew/opt/fdk-aac || echo /usr/local/opt/fdk-aac)
        say "running the ELD encoder tests (host fdk-aac: $host_fdk)"
        if ! FDK_AAC_PREFIX="$host_fdk" cargo test -p eld-codec; then
            echo "eld-codec tests FAILED — the encoder configuration is wrong. This is what the" >&2
            echo "SBR regression looked like: a correct-looking uplink that iOS silently discards." >&2
            exit 1
        fi
    else
        say "no host fdk-aac — SKIPPING the ELD encoder tests (brew install fdk-aac to enable)"
    fi

    say "building airplayd WITH mic-uplink-eld (fdk-aac: $FDK_PREFIX)"
    cargo build --release --target "$TARGET" -p airplayd
    if strings "target/$TARGET/release/airplayd" | grep -q 'mic-uplink-eld` not built'; then
        echo "airplayd still reports mic-uplink-eld not built — the feature did NOT compile in" >&2
        exit 1
    fi
else
    say "building airplayd WITHOUT mic-uplink-eld — no libfdk-aac at $FDK_PREFIX"
    say "  MIC UPLINK WILL BE SILENT. Build it with pi/tools/build_fdk_aac_arm64.sh"
    cargo build --release --target "$TARGET" -p airplayd --no-default-features
fi

OUT="target/$TARGET/release"
for b in carplay-wireless airplayd; do
    file "$OUT/$b" | grep -q "ARM aarch64" || { echo "$b is not aarch64!" >&2; exit 1; }
done
say "built:"
ls -l "$OUT/carplay-wireless" "$OUT/airplayd"

[ "$PUSH" -eq 1 ] || { say "--no-push: stopping here"; exit 0; }

# ---- push --------------------------------------------------------------------------------------

say "pushing (rename-aside, so a live session is undisturbed)"
for b in carplay-wireless airplayd; do
    "${adb[@]}" push "$OUT/$b" "$TMP/$b.new" >/dev/null
    "${adb[@]}" shell "chmod 755 $TMP/$b.new"
    # Rename rather than overwrite: overwriting a running executable is ETXTBSY, renaming is fine
    # and the running process keeps its own inode.
    "${adb[@]}" shell "mv -f $TMP/$b $TMP/$b.old 2>/dev/null; mv -f $TMP/$b.new $TMP/$b"
    say "  $b -> $TMP/$b (previous kept as $b.old)"
done

say "verifying the new binaries carry the new code"
"${adb[@]}" shell "strings $TMP/carplay-wireless | grep -c 'device management on'" | tr -d '\r' | \
    { read -r n; [ "$n" -ge 1 ] && say "  carplay-wireless: control socket present" || say "  WARNING: control socket string NOT found"; }
"${adb[@]}" shell "strings $TMP/airplayd | grep -c 'CARPLAY_CFG_FILE'" | tr -d '\r' | \
    { read -r n; [ "$n" -ge 1 ] && say "  airplayd: CARPLAY_CFG_FILE override present" || say "  WARNING: CARPLAY_CFG_FILE NOT found"; }

cat <<EOF

Pushed, but NOT running yet — the processes on the device are still the old binaries.
To adopt them (this DROPS any live CarPlay session):

  pi/tools/start_stack.sh --serial ${SERIAL:-<serial>} --restart

Then confirm the new pieces:
  adb shell 'ss -ltn | grep 9115'                 # device-management control socket
  adb shell 'grep -m1 "^\[airplayd\] pairing" $TMP/wireless.log'   # logs its config path
EOF
