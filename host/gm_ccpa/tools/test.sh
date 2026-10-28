#!/bin/bash
# Tier-0 verification gate (docs/11_HARDENING_PLAN.md): host-side cargo tests for the vendored
# protocol crates. No hardware needed. Run before packaging any APK.
#
# The vendored crates are path deps, not members of the carplay-jni workspace, so they must be tested
# from their own directories. receiver must be built with --no-default-features --features mic-uplink
# to dodge the eld-codec / fdk-aac feature-unification trap (docs/06 §6.3, item 3).
# Repo-relative roots. This project lives at ccpa_custom/host/gm_ccpa, so the tools resolve both
# roots from their own location rather than hard-coding an absolute path — moving the checkout, or
# having a second one, must not silently build against the wrong tree.
GM_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")/.." && pwd)"
CCPA_ROOT="$(cd "$GM_ROOT/../.." && pwd)"

set -euo pipefail
export PATH="$HOME/.cargo/bin:$PATH"
# No tools/__pycache__ from the python steps below: a bytecode cache in the tree is an untracked
# directory that dirties the build stamp (E-F4) and an artifact the guard below would then name.
export PYTHONDONTWRITEBYTECODE=1

# ---- in-tree build-artifact guard (owner rule, 2026-10-08) ------------------------------------------
# FIRST, before anything builds: a real `target`/`build`/`.gradle`/`__pycache__`/… directory inside
# host/gm_ccpa or crates/ fails the gate (tools/tree_artifact_guard.sh has the rationale and the
# GMCCPA_ALLOW_TREE_ARTIFACTS=1 escape for a run made while cleaning up).
echo "=== Tier-0: in-tree build-artifact guard ==="
bash "$GM_ROOT/tools/tree_artifact_guard.sh" "$GM_ROOT" "$CCPA_ROOT/crates"

CCPA="$CCPA_ROOT/crates/vendor"

# Every cargo invocation below writes OUTSIDE ~/Documents (ccpa_custom/CLAUDE.md "Build artifacts and
# caches never live under ~/Documents": iCloud drops conflict copies into build trees). The vendored
# crates are not workspace members, so without this each `cargo test` created its own in-tree
# crates/vendor/<crate>/target (≈1.1 GB between them). The JNI cross-build uses build_apk.sh's exact
# directory so the two share one fingerprint tree; the host test builds get their own.
CARGO_CACHE="$HOME/.cache/cargo-targets"
JNI_TARGET_DIR="${CARGO_TARGET_DIR:-$CARGO_CACHE/gm_ccpa-carplay-jni}"

echo "=== Tier-0: receiver ==="
( cd "$CCPA/receiver" && CARGO_TARGET_DIR="$CARGO_CACHE/gm_ccpa-gate-receiver" \
  cargo test --no-default-features --features mic-uplink )
# Plan v5.8 V1a (VA): the app's feature set — the :9003 voice-tag v2 wire, its golden vectors, and the
# serialized end-to-end tests/voice_seam.rs (spawn_audio on a per-test Sink and ephemeral ports).
echo "=== Tier-0: receiver + voice-tag-v2 (incl. tests/voice_seam.rs) ==="
( cd "$CCPA/receiver" && CARGO_TARGET_DIR="$CARGO_CACHE/gm_ccpa-gate-receiver" \
  cargo test --no-default-features --features mic-uplink,voice-tag-v2 )

echo "=== Tier-0: pairing ==="
( cd "$CCPA/pairing" && CARGO_TARGET_DIR="$CARGO_CACHE/gm_ccpa-gate-pairing" cargo test )

# The app's own JNI crate on the HOST (F141 peer-store cap, the A-F2 logcat line classifier). Its own
# target dir: the cross-build below shares build_apk.sh's, and a host test build must not touch that
# tree's fingerprints. --no-default-features: the default set needs the cross-built libfdk-aac.
echo "=== Tier-0: carplay-jni host tests ==="
( cd "$GM_ROOT/native/carplay-jni" && CARGO_TARGET_DIR="$CARGO_CACHE/gm_ccpa-jni-host-tests" \
  cargo test --no-default-features )

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
  && CARGO_TARGET_DIR="$JNI_TARGET_DIR" \
     CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER="$NDK_BIN/x86_64-linux-android32-clang" \
     RUSTFLAGS="--remap-path-prefix=$HOME=~" \
     cargo build --locked --release --target x86_64-linux-android )
# --locked as in build_apk.sh: a Cargo.lock that needs rewriting is a gate failure, not a silent edit.
# RUSTFLAGS and CARGO_TARGET_DIR above are build_apk.sh's values, verbatim (plan v5.8 V6 / Q022).
# RUSTFLAGS strips host paths from the .so. Cargo's fingerprints include RUSTFLAGS, so the gate and
# build_apk.sh can share one target tree only if both use the identical value — a different value
# here would make every alternation between this gate and a real build a full native rebuild.

# ---- JVM unit tests (plan v5.8 rev d: the Kotlin tests join tier-0) --------------------------------
# kotlinc + JUnit 4 from the Gradle cache, no Gradle — the same recipe as the per-unit it_v57_*.sh
# scripts. The class list and its format live in tools/jvm_tests.txt. android.jar / android.car.jar
# are on the COMPILE classpath only; the runtime classpath has no Android, so a test that reaches a
# real Android call fails loudly — except the two runtime stubs in tools/jvm_stubs. A missing toolchain, an empty list or a listed class without its
# source is a FAIL, never a skip: a gate that runs zero tests is a silent green.
echo "=== Tier-0: JVM unit tests (tools/jvm_tests.txt) ==="
JVM_LIST="$GM_ROOT/tools/jvm_tests.txt"
APP_SRC="$GM_ROOT/netprobe_app/app/src"
SDK="$HOME/Library/Android/sdk"
ANDJAR="$SDK/platforms/android-32/android.jar"
CARJAR="$SDK/platforms/android-32/optional/android.car.jar"
KC="/Applications/Android Studio.app/Contents/plugins/Kotlin/kotlinc/bin/kotlinc"
STDLIB="/Applications/Android Studio.app/Contents/plugins/Kotlin/kotlinc/lib/kotlin-stdlib.jar"
GRADLE_FILES="$HOME/.gradle/caches/modules-2/files-2.1"
JUNIT="$(find "$GRADLE_FILES/junit/junit/4.13.2" -name 'junit-4.13.2.jar' 2>/dev/null | head -1 || true)"
HAMCREST="$(find "$GRADLE_FILES/org.hamcrest" -name 'hamcrest-core-1.3.jar' 2>/dev/null | head -1 || true)"
for p in "$JVM_LIST" "$ANDJAR" "$CARJAR" "$KC" "$STDLIB" "$JUNIT" "$HAMCREST"; do
    [ -n "$p" ] && [ -e "$p" ] || {
        echo "[Tier-0] FAIL: JVM test input missing: '${p}' (junit 4.13.2 / hamcrest-core 1.3 come"
        echo "  from the Gradle cache — one Android Studio sync of netprobe_app fetches them)."
        exit 1
    }
done
command -v java >/dev/null || { echo "[Tier-0] FAIL: no java on PATH"; exit 1; }
JVM_CLASSES=()
JVM_SRCS=()
JVM_SEEN=$'\n'
JVM_LINENO=0
while IFS= read -r line || [ -n "$line" ]; do
    JVM_LINENO=$((JVM_LINENO + 1))
    line="${line%%#*}"
    line="${line#"${line%%[![:space:]]*}"}"      # trim leading whitespace
    line="${line%"${line##*[![:space:]]}"}"      # trim trailing whitespace (incl. a CR)
    [ -n "$line" ] || continue
    if ! [[ "$line" =~ ^[A-Za-z_][A-Za-z0-9_]*(\.[A-Za-z_][A-Za-z0-9_]*)+$ ]]; then
        echo "[Tier-0] FAIL: tools/jvm_tests.txt:$JVM_LINENO: not a class name: '$line'"; exit 1
    fi
    case "$JVM_SEEN" in *$'\n'"$line"$'\n'*)
        echo "[Tier-0] FAIL: tools/jvm_tests.txt:$JVM_LINENO: duplicate class $line"; exit 1 ;;
    esac
    JVM_SEEN="$JVM_SEEN$line"$'\n'
    src="$APP_SRC/test/java/${line//.//}.kt"
    [ -f "$src" ] || {
        echo "[Tier-0] FAIL: tools/jvm_tests.txt:$JVM_LINENO: $line has no source at ${src#$GM_ROOT/}"
        exit 1
    }
    JVM_CLASSES+=("$line")
    JVM_SRCS+=("$src")
done < "$JVM_LIST"
[ "${#JVM_CLASSES[@]}" -gt 0 ] || { echo "[Tier-0] FAIL: tools/jvm_tests.txt lists no classes"; exit 1; }
JVM_OUT="$(mktemp -d -t gmccpa_jvmtest)"
# src/nobench/java: main references zeno.gmccpa.bench.BenchHooks (MainActivity's mic_inject verb);
# tier-0 compiles the RELEASE flavor's stub, as a default build_apk.sh does (plan v5.9 MIC).
echo "    ${#JVM_CLASSES[@]} classes; compiling main + nobench + tests into $JVM_OUT"
if ! "$KC" -jvm-target 1.8 -classpath "$JUNIT:$ANDJAR:$CARJAR" -d "$JVM_OUT/classes" \
        "$APP_SRC/main/java" "$APP_SRC/nobench/java" "${JVM_SRCS[@]}" > "$JVM_OUT/kotlinc.log" 2>&1; then
    echo "[Tier-0] FAIL: kotlinc (JVM unit tests)"
    grep -E '(^|: )error:' "$JVM_OUT/kotlinc.log" | head -20 || true
    echo "  Full output: $JVM_OUT/kotlinc.log"
    exit 1
fi
# Runtime-only Android stubs (tools/jvm_stubs: a recording android.util.Log, a throwing
# android.os.SystemClock), AFTER the app classes. Nothing else from Android is on this classpath.
JVM_STUBS_SRC="$GM_ROOT/tools/jvm_stubs"
find "$JVM_STUBS_SRC" -name '*.java' > "$JVM_OUT/stubs.list"
[ -s "$JVM_OUT/stubs.list" ] || { echo "[Tier-0] FAIL: no stubs under ${JVM_STUBS_SRC#$GM_ROOT/}"; exit 1; }
javac -nowarn -d "$JVM_OUT/stubs" @"$JVM_OUT/stubs.list"
if ! java -cp "$JVM_OUT/classes:$JVM_OUT/stubs:$JUNIT:$HAMCREST:$STDLIB" org.junit.runner.JUnitCore \
        "${JVM_CLASSES[@]}" > "$JVM_OUT/junit.log" 2>&1; then
    echo "[Tier-0] FAIL: JUnit"
    grep -E '^[0-9]+\) |^Tests run:|^FAILURES' "$JVM_OUT/junit.log" | head -40 || true
    echo "  Full output: $JVM_OUT/junit.log"
    exit 1
fi
grep -E '^OK \(' "$JVM_OUT/junit.log"

# D-F1 (plan 2.4): no two PendingIntent.getActivity(...) sites targeting MainActivity may share a request
# code — extras are not part of a PendingIntent's key. Textual, so it fails before the unit does.
echo "=== Tier-0: PendingIntent request-code lint ==="
python3 -I -B "$GM_ROOT/tools/pending_intent_lint.py" "$APP_SRC/main/java" || exit 1

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
# ccpa_custom's checker parses ocbm-proto as canonical and diffs every client against it — this app's
# OcbmProto.kt is an app-owned fork (2026-09-11, no longer a symlink into CarlinkAndroid), so it is a
# real third client and the checker covers it with no arguments. A value disagreement is an error; a
# constant this app has not defined is a GAP and only fails for the core set (channel ids, CT_*
# opcodes, frame flags). The 2 gaps reported for THIS client today are correct and expected:
# CMD_VIEW_AREA (0x11) is a box-side opcode this app never drives — its view areas are requested by
# iOS directly over AirPlay (CarPlayActivity "requestViewArea"), never through the box's CH_INPUT —
# and F_BOTH is a client-side convenience alias for F_SOM|F_EOM that ocbm-proto has no need to name.
# (The pre-fork count quoted here was 57, from back when this path resolved through CarlinkAndroid's
# copy and the checker's totals covered every client at once.)
#
# SKIPPED, not failed, when the checker is absent (a gm_ccpa subtree exported on its own must stay
# buildable). No root argument: the checker rejects one since 2026-09-11.
PROTO_CHECK="$CCPA_ROOT/tools/proto_check.py"
if [ -x "$PROTO_CHECK" ]; then
    echo "=== Tier-0: OCBM protocol conformance ==="
    # Output under this run's mktemp dir, not a fixed /tmp path, so two gate runs cannot clobber
    # each other's report.
    if ! python3 -I -B "$PROTO_CHECK" > "$JVM_OUT/proto_check.out" 2>&1; then
        echo "[Tier-0] FAIL: OCBM client disagrees with crates/ocbm-proto."
        grep -E "^(error|ERROR)" "$JVM_OUT/proto_check.out" | head -20
        echo "  Full output: $JVM_OUT/proto_check.out"
        echo "  ocbm-proto is canonical. Fix the client, not the spec."
        exit 1
    fi
    tail -1 "$JVM_OUT/proto_check.out"
else
    echo "=== Tier-0: OCBM protocol conformance (SKIPPED — no ccpa_custom checkout) ==="
fi

echo "[Tier-0] all green"
