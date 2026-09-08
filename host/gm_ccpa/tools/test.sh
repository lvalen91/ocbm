#!/bin/bash
# Tier-0 verification gate (docs/11_HARDENING_PLAN.md): host-side cargo tests for the vendored
# protocol crates. No hardware needed. Run before packaging any APK.
#
# The vendored crates are path deps, not members of the carplay-jni workspace, so they must be tested
# from their own directories. receiver must be built with --no-default-features --features mic-uplink
# to dodge the eld-codec / fdk-aac feature-unification trap (docs/06 §5d).
# Repo-relative roots. This project lives at ccpa_custom/host/gm_ccpa, so the tools resolve both
# roots from their own location rather than hard-coding an absolute path — moving the checkout, or
# having a second one, must not silently build against the wrong tree.
GM_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")/.." && pwd)"
CCPA_ROOT="$(cd "$GM_ROOT/../.." && pwd)"

set -euo pipefail
export PATH="$HOME/.cargo/bin:$PATH"

CCPA="$CCPA_ROOT/crates/vendor"

echo "=== Tier-0: receiver ==="
( cd "$CCPA/receiver" && cargo test --no-default-features --features mic-uplink )

echo "=== Tier-0: pairing ==="
( cd "$CCPA/pairing" && cargo test )

echo "=== Tier-0: carplay-jni builds for the head unit ABI ==="
NDK_BIN="$HOME/Library/Android/sdk/ndk/30.0.15729638/toolchains/llvm/prebuilt/darwin-x86_64/bin"
# eld-codec compiles a C shim against libfdk-aac, so this gate needs the SAME cross-toolchain env as
# build_apk.sh. Without it the gate fails on `cc failed compiling csrc/eld_shim.c` even though the
# real build succeeds — a false red that would train people to ignore the gate.
( cd "$(dirname "${BASH_SOURCE[0]}")/../native/carplay-jni" \
  && FDK_AAC_PREFIX="${FDK_AAC_PREFIX:-$CCPA_ROOT/scratchpad/fdk/install-android-x86_64}" \
  && CC_x86_64_linux_android="$NDK_BIN/x86_64-linux-android32-clang" \
  && AR_x86_64_linux_android="$NDK_BIN/llvm-ar" \
  && export FDK_AAC_PREFIX CC_x86_64_linux_android AR_x86_64_linux_android \
  && CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER="$NDK_BIN/x86_64-linux-android32-clang" \
     cargo build --release --target x86_64-linux-android )

# ---- documentation budget -----------------------------------------------------------------------
# A hard cap, enforced rather than merely stated. Doc directories rot by accretion: each session adds
# a dated write-up, none are ever retired, and a later reader cannot tell which of five documents
# describing the same thing is current. The cap forces the choice — update in place, or retire
# something — at the moment the new file is written, which is the only moment anyone has the context
# to decide. Raw per-session captures belong in evidence/, not docs/.
DOCS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../docs" && pwd)"
DOC_COUNT=$(find "$DOCS_DIR" -maxdepth 1 -name '*.md' | wc -l | tr -d ' ')
DOC_MAX=10
echo "=== Tier-0: documentation budget ($DOC_COUNT/$DOC_MAX) ==="
if [ "$DOC_COUNT" -gt "$DOC_MAX" ]; then
    echo "[Tier-0] FAIL: docs/ holds $DOC_COUNT files, budget is $DOC_MAX."
    echo "  Fold the new material into an existing document, or retire one that is superseded."
    echo "  See 'Document policy' in docs/00_HANDOFF.md."
    exit 1
fi

# --- OCBM protocol conformance -------------------------------------------------------------------
#
# This app's Kotlin client and the box's `crates/ocbm-proto` are two ends of one wire. Nothing
# structural stops them drifting: the constants are hand-maintained on both sides, and a value that
# disagrees is not a compile error anywhere — it is a frame the other end misreads at runtime.
#
# ccpa_custom's checker parses ocbm-proto as canonical and diffs every client against it. A value
# disagreement is an error; a constant this app has not defined is a GAP and only fails for the core
# set (channel ids, CT_* opcodes, frame flags). The 57 gaps reported today are correct and expected:
# gm_ccpa defines no INPUT_*/NAV_*/TOUCH_* because its phone is on WiFi and touch goes direct over
# AirPlay HID via nativeTouch, never through the box's CH_INPUT; and no IP_* because it does not drive
# the CH_IP stream mux.
#
# SKIPPED, not failed, when the sibling checkout is absent: this repo must stay buildable without it.
PROTO_CHECK="$CCPA_ROOT/tools/proto_check.py"
if [ -x "$PROTO_CHECK" ]; then
    echo "=== Tier-0: OCBM protocol conformance ==="
    REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
    if ! python3 "$PROTO_CHECK" "$REPO_ROOT" > /tmp/proto_check.out 2>&1; then
        echo "[Tier-0] FAIL: OCBM client disagrees with crates/ocbm-proto."
        grep -E "^(error|ERROR)" /tmp/proto_check.out | head -20
        echo "  Full output: /tmp/proto_check.out"
        echo "  ocbm-proto is canonical. Fix the client, not the spec."
        exit 1
    fi
    tail -1 /tmp/proto_check.out
else
    echo "=== Tier-0: OCBM protocol conformance (SKIPPED — no ccpa_custom checkout) ==="
fi

echo "[Tier-0] all green"
