#!/bin/bash
# Gradle-free debug APK build for netprobe (single Kotlin file, no app resources).
# Repo-relative roots. This project lives at ccpa_custom/host/gm_ccpa, so the tools resolve both
# roots from their own location rather than hard-coding an absolute path — moving the checkout, or
# having a second one, must not silently build against the wrong tree.
GM_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")/.." && pwd)"
CCPA_ROOT="$(cd "$GM_ROOT/../.." && pwd)"

set -euo pipefail
# No tools/__pycache__ if a python helper is ever invoked from here (same rule as tools/test.sh).
export PYTHONDONTWRITEBYTECODE=1

SDK="$HOME/Library/Android/sdk"
BT="$SDK/build-tools/37.0.0"
# Compile against the head unit's ACTUAL platform (API 32). Compiling against android-35 lets the
# compiler resolve API 33-35 symbols that do not exist on gminfo37 -> NoSuchMethodError at runtime,
# not a build error. The unit is API 32; compile against API 32.
ANDJAR="$SDK/platforms/android-32/android.jar"
# The Car API (CarPropertyManager, CarUxRestrictionsManager) is NOT in android.jar — it ships as the
# `android.car` shared library, stubbed here. COMPILE/LINK ONLY: it is provided by the platform at
# runtime via `<uses-library android:name="android.car">`, so it must never reach a d8 *input* or the
# APK would carry a duplicate copy of framework classes and fail to resolve against the real one.
CARJAR="$SDK/platforms/android-32/optional/android.car.jar"
KC="/Applications/Android Studio.app/Contents/plugins/Kotlin/kotlinc/bin/kotlinc"
STDLIB="/Applications/Android Studio.app/Contents/plugins/Kotlin/kotlinc/lib/kotlin-stdlib.jar"

# Fail fast with a clear message if any pinned toolchain path is missing (an Android Studio / SDK
# update otherwise breaks the build mid-script with a cryptic error).
for p in "$BT/d8" "$BT/aapt2" "$BT/zipalign" "$BT/apksigner" "$ANDJAR" "$CARJAR" "$KC" "$STDLIB"; do
    [ -e "$p" ] || { echo "FATAL: required toolchain path missing: $p" >&2; exit 1; }
done
# kotlinc is whatever Android Studio bundles and is otherwise unpinned. Report drift (PRINT, not fail:
# a Studio update must not block a build) so a bytecode difference between two APKs can be traced to
# the compiler. Override KOTLINC_EXPECTED to re-pin after a deliberate upgrade.
KOTLINC_EXPECTED="${KOTLINC_EXPECTED:-2.3.10}"
KOTLINC_ACTUAL="$("$KC" -version 2>&1 | sed -n 's/^info: kotlinc-jvm \([0-9][0-9.]*\).*/\1/p' | head -1)"
if [ "$KOTLINC_ACTUAL" != "$KOTLINC_EXPECTED" ]; then
    echo "WARNING: kotlinc is ${KOTLINC_ACTUAL:-unknown}, expected $KOTLINC_EXPECTED — bytecode may differ from earlier builds" >&2
fi

PROJ="$GM_ROOT/netprobe_app"
# Compile the whole java/ source root. Every source class is zeno.gmccpa.* (one tree). The INSTALL
# package is a separate thing — INSTALL_PKG below — and does not move any class. Kept as a source-root
# compile rather than a package glob so a new subpackage never has to be added here to get built.
SRCDIR="$PROJ/app/src/main/java"
MAN="$PROJ/app/src/main/AndroidManifest.xml"
# INSTALL PACKAGE = android.car.usb.handler, DELIBERATELY, again (owner request 2026-10-15: recent
# bench testing used that package name). It is the package GM's config_UsbDeviceConnectionHandling_component
# names (ccpa_custom/docs/host/01_ANDROID_AND_AAOS.md); it was this app's package until 2026-09-08,
# then zeno.gmccpa until 2026-10-15. The manifest keeps package="zeno.gmccpa" (= the Gradle namespace,
# the class package) and `aapt2 link --rename-manifest-package` below sets the installed id, exactly as
# Gradle's applicationId does (netprobe_app/app/build.gradle carries the same value). Classes, JNI
# symbol names (Java_zeno_gmccpa_*) and every fully-qualified component android:name are unaffected.
# Short am forms resolve against THIS name: use android.car.usb.handler/zeno.gmccpa.MainActivity,
# never android.car.usb.handler/.MainActivity.
INSTALL_PKG="android.car.usb.handler"

# ---- flavor: release (default) or bench (plan v5.9 MIC) -----------------------------------------
# Exactly ONE of src/nobench/java and src/bench/java is compiled beside src/main/java. They hold the
# same class, zeno.gmccpa.bench.BenchHooks: the nobench stub only logs "not in this build (release)";
# the bench one is the `mic_inject` operator verb (the only caller of MicInject.arm). GMCCPA_BENCH=1
# selects bench and marks the artifact — versionName, assets/build_sha line 1 and the filename all
# carry `-bench` — so a bench APK can never pass for a release (V6 identity). Unset or 0 = release.
# Any other value is FATAL: a typo must not pick a flavor silently.
case "${GMCCPA_BENCH:-0}" in
    1)    FLAVOR=bench;   FLAVOR_SFX="-bench" ;;
    0|"") FLAVOR=nobench; FLAVOR_SFX="" ;;
    *)    echo "FATAL: GMCCPA_BENCH must be 1, 0 or unset (got '${GMCCPA_BENCH}')" >&2; exit 1 ;;
esac
FLAVOR_SRC="$PROJ/app/src/$FLAVOR/java"
[ -d "$FLAVOR_SRC" ] || { echo "FATAL: flavor source dir missing: $FLAVOR_SRC" >&2; exit 1; }

# ---- build identity (plan v5.8 V6 / Q009) -------------------------------------------------------
# ONE predicate and ONE stamp, computed once, BEFORE anything is compiled, and reused for the
# build_sha asset, the versionName and the output filename. Two things were wrong before:
#   * `git diff --quiet HEAD` ignores untracked files, so a build whose only change was a NEW .kt
#     source (33 of them existed at review time) stamped as clean;
#   * it ran from GM_ROOT (host/gm_ccpa), while carplay-jni compiles crates/vendor/* from outside
#     that subtree — and the filename had its own, second copy of the predicate.
# And a bare `-dirty` suffix gave every dirty build the same name: rc1 and rc2 both shipped as
# versionCode 420 / `4.0+cac07fa-dirty`, so nothing on the unit said which sources it ran.
#
# Rule: TOP = the repository top level. The tree is dirty when `git status --porcelain` there is
# non-empty — untracked, non-ignored files count (-unormal pins that against a
# status.showUntrackedFiles=no config). Dirty → STAMP = <sha>-dirty.<h8>, where h8 is the first 8
# hex digits of sha256 over `git diff HEAD --binary` plus every untracked, non-ignored file (path
# and content, path-sorted). Same tree → same h8; any edit or new file → a new h8. --binary implies
# --full-index, so the hashed text does not drift as the object store grows (auto-abbrev would).
# Ignored files (scratchpad/, target/, apk/) are outside the stamp by definition.
# Every git failure is FATAL rather than read as "clean": an unknown tree must never stamp clean.
stamp_delta() {   # stdout: the byte stream h8 is the hash of
    local top="$1" f p d
    git -C "$top" diff HEAD --binary --no-ext-diff --no-textconv --no-color || return 1
    git -C "$top" ls-files -o --exclude-standard -z > "$STAMP_TMP" || return 1
    # One read per file (its digest), so a file can never be framed with one size and hashed with
    # other content. Symlinks hash their target text; anything else (a nested repo shows up as
    # `dir/`) hashes its path only. Each step is checked explicitly: errexit is not dependable
    # inside a command substitution, and a file that vanished mid-hash must fail, not hash as "".
    while IFS= read -r -d '' f; do
        p="$top/$f"
        if [ -L "$p" ]; then
            d="$(readlink "$p")" || return 1
            printf 'L\0%s\0%s\0' "$f" "$d"
        elif [ -f "$p" ]; then
            d="$(shasum -a 256 < "$p")" || return 1
            printf 'F\0%s\0%s\0' "$f" "${d%% *}"
        else
            printf 'O\0%s\0' "$f"
        fi
    done < <(LC_ALL=C sort -z "$STAMP_TMP")
}
TOP="$(git -C "$GM_ROOT" rev-parse --show-toplevel 2>/dev/null || true)"
if [ -z "$TOP" ]; then
    STAMP="nogit"
else
    STAMP="$(git -C "$TOP" rev-parse --short HEAD)"
    PORCELAIN="$(git -C "$TOP" status --porcelain --untracked-files=normal)"
    if [ -n "$PORCELAIN" ]; then
        STAMP_TMP="$(mktemp -t gmccpa_stamp)"
        H8="$(stamp_delta "$TOP" | shasum -a 256 | cut -c1-8)"
        rm -f "$STAMP_TMP"
        [[ "$H8" =~ ^[0-9a-f]{8}$ ]] || { echo "FATAL: dirty-tree hash failed (got '$H8')" >&2; exit 1; }
        STAMP="$STAMP-dirty.$H8"
    fi
fi
BUILD_UTC="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo "[build] identity: $STAMP (built $BUILD_UTC)"
echo "[build] flavor: $FLAVOR${FLAVOR_SFX:+ (versionName/build_sha/filename suffix $FLAVOR_SFX)}"

WORK="$(mktemp -d -t gmccpa_build)"
echo "[build] work dir: $WORK"
mkdir -p "$WORK/classes" "$WORK/dex"

echo "[1/6] kotlinc compile"
# A compile failure must NOT fall through to packaging: with `|| true` the script happily shipped an
# APK built from whatever classes survived, which looks like a successful build of stale code.
# pipefail is already on from `set -euo pipefail`; do NOT disable it (a failed kotlinc piped to sed
# would otherwise look like success), so it stays on for every pipeline below too.
"$KC" -jvm-target 1.8 -classpath "$ANDJAR:$CARJAR" -d "$WORK/classes" "$SRCDIR" "$FLAVOR_SRC" 2>&1 | sed 's/^/    /'
# jar the compiled classes
( cd "$WORK/classes" && jar cf "$WORK/app-classes.jar" . )

echo "[2/6] d8 -> classes.dex"
"$BT/d8" --min-api 32 --lib "$ANDJAR" --lib "$CARJAR" --output "$WORK/dex" \
    "$WORK/app-classes.jar" "$STDLIB"

echo "[3/6] aapt2 compile+link (res/xml/device_filter.xml is referenced by the USB intent-filter)"
RES="$PROJ/app/src/main/res"
LINK_RES=()
if [ -d "$RES" ]; then
    "$BT/aapt2" compile --dir "$RES" -o "$WORK/res.zip"
    LINK_RES=("$WORK/res.zip")
fi
ASSETS="$PROJ/app/src/main/assets"
# Stamp the build SHA into an asset so the RUNNING app can print it.
# It is otherwise unreachable at runtime: the sha lives only in the APK FILENAME, and `pm install`
# discards the filename, while this Gradle-free build generates no BuildConfig. Without it a
# logcat from the truck cannot be tied to the sources that produced it — which is the whole point
# of stamping the filename in the first place. Written into a COPY of the assets dir so the source
# tree stays clean and a dirty asset never shows up in `git status`.
# Line 1 = STAMP (see "build identity" above), line 2 = the UTC build time.
ASSET_ARG=()
if [ -d "$ASSETS" ]; then
    cp -R "$ASSETS" "$WORK/assets"
    printf '%s\n%s\n' "$STAMP$FLAVOR_SFX" "$BUILD_UTC" > "$WORK/assets/build_sha"
    ASSET_ARG=(-A "$WORK/assets")
    echo "    stamped assets/build_sha = $STAMP$FLAVOR_SFX / $BUILD_UTC"
fi
# versionCode/versionName are INJECTED here, not written in the manifest — `aapt2 link` only honours
# --version-code when the manifest carries none, so the attributes were removed from AndroidManifest.xml
# on 2026-09-11 to hand this script the authority. The code is the commit count, which is monotonic
# because this repo has one linear branch (see ccpa_custom/CLAUDE.md "One branch: main"), needs no
# state file, and cannot collide. The name carries the sha so `dumpsys package $INSTALL_PKG` names the
# exact sources on the unit — which a pinned 7/"4.0" never could.
#
# COST, accepted deliberately: older artifacts in apk/ are all versionCode 7, and this APK is NOT
# debuggable, so `adb install -d` cannot roll back to one. Rolling back needs
# `adb uninstall -k $INSTALL_PKG` first (zeno.gmccpa for a 2026-09-08..10-15 build) — the -k matters, a plain uninstall wipes carplay_peers.bin and
# leaves the box's BR/EDR bond asserting a pairing the app no longer has (the split brain
# BoxAction.FORGET_PHONE exists to repair). Do not "fix" this by marking the app debuggable: on API 32
# `adb backup` eligibility for a non-privileged app is decided by FLAG_DEBUGGABLE, not allowBackup, so
# that would re-open the log-extraction surface the manifest just closed.
#
# Every dirty build on one commit shares that commit's code, and `install -r` over an EQUAL code is
# accepted — so between those builds identity rides on versionName and assets/build_sha (both carry
# STAMP, i.e. the dirty-tree hash), never on the code (plan v5.8 V6).
VCODE="$(cd "$GM_ROOT" && git rev-list --count HEAD)" \
    || { echo "FATAL: git rev-list --count HEAD failed — versionCode is the commit count, nothing else" >&2; exit 1; }
VNAME="4.0+$STAMP$FLAVOR_SFX"
echo "    versionCode=$VCODE versionName=$VNAME"
"$BT/aapt2" link -o "$WORK/base.apk" -I "$ANDJAR" \
    --manifest "$MAN" --rename-manifest-package "$INSTALL_PKG" \
    --min-sdk-version 32 --target-sdk-version 32 \
    --version-code "$VCODE" --version-name "$VNAME" \
    "${ASSET_ARG[@]+${ASSET_ARG[@]}}" \
    --auto-add-overlay ${LINK_RES[@]+"${LINK_RES[@]}"}

echo "[3.5/6] build + add native lib (lib/x86_64)"
# The .so used to be COPIED-IF-PRESENT: edit Kotlin, forget `cargo build`, and this silently
# repackaged a stale native lib; if it was missing entirely it printed a WARNING and produced a
# signed, installable, PAIRING-INCAPABLE apk. `set -e` does not catch an if/else. Build it here and
# hard-fail, so the artifact can never disagree with the sources it was built from.
# libfdk-aac for the mic-uplink AAC-ELD encoder (feature `mic-uplink-eld`). Cross-built for
# x86_64-linux-android with the NDK; see docs/13 §4. Without this the encoder is silently omitted
# and the mic advertises three ELD formats it cannot serve.
export FDK_AAC_PREFIX="${FDK_AAC_PREFIX:-$CCPA_ROOT/scratchpad/fdk/install-android-x86_64}"
if [ ! -f "$FDK_AAC_PREFIX/lib/libfdk-aac.a" ]; then
    echo "FATAL: libfdk-aac not found at $FDK_AAC_PREFIX — the ELD mic encoder cannot build." >&2
    echo "       Rebuild it (docs/13 §4) or unset the mic-uplink-eld feature in Cargo.toml." >&2
    exit 1
fi
NATIVE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/native/carplay-jni"
# Cargo output goes OUTSIDE ~/Documents, which is iCloud "Desktop & Documents" synced on this Mac.
# iCloud resolves its own write races by dropping conflict copies beside the original — `classes 2.jar`,
# `Foo$Bar 2.dex` — and a toolchain then consumes them as real inputs, giving nondeterministic
# duplicate-class and wrong-filename failures that a `clean` only fixes until sync catches up. A build
# tree is exactly the wrong thing to sync: hundreds of megabytes, rewritten constantly, worthless off
# this machine. Same convention as the repo root, whose `target` is symlinked to the same cache root.
# Override with CARGO_TARGET_DIR=... for a one-off build elsewhere.
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/cargo-targets/gm_ccpa-carplay-jni}"
JNILIB="$CARGO_TARGET_DIR/x86_64-linux-android/release/libcarplayjni.so"
NDK_BIN="$HOME/Library/Android/sdk/ndk/30.0.15729638/toolchains/llvm/prebuilt/darwin-x86_64/bin"
# CC/AR must be set, not just the LINKER. eld-codec's build.rs compiles csrc/eld_shim.c with the cc
# crate, which honours CC_<target> and otherwise falls back to the HOST compiler. Setting only the
# linker let it build the shim as a Mach-O arm64 object, which LLD then linked into the ELF without
# complaint — producing a .so with eld_enc_* left UNDEFINED. Those are GLOBAL symbols with GLOB_DAT
# relocations in .rela.dyn and the lib is BIND_NOW, so dlopen fails outright:
#   UnsatisfiedLinkError: dlopen failed: cannot locate symbol "eld_enc_encode"
# NativeCore.available then goes false: 0 = no core, and every control connection is closed (there is no
# fallback since v5.9 UC1) — a total CarPlay failure whose only symptom is one log line. tools/test.sh already sets
# these for the same reason; the two must stay in lockstep.
# RUSTFLAGS --remap-path-prefix (plan v5.8 V6 / Q022): without it the release .so embedded 82 host
# paths (58 under /Users/<name>/.cargo/registry/src, 24 under /Users/<name>/.rustup/toolchains)
# — the build machine's account name and layout, shipped to the head unit. tools/test.sh builds
# the same crate into the same CARGO_TARGET_DIR (since 2026-10-08; before that it built in-tree), and
# cargo's fingerprints include RUSTFLAGS, so the two can share that tree only while both pass the
# identical value — a different value in either would make every alternation a full native
# rebuild. The repo .cargo/config.toml sets no x86_64-linux-android rustflags, so this env var
# overrides nothing there.
# --locked: Cargo.lock is tracked, and the APK's identity is its sha — a silent lockfile rewrite
# would ship different crate versions under the same stamp.
( cd "$NATIVE_DIR" \
  && PATH="$HOME/.cargo/bin:$PATH" \
     RUSTFLAGS="--remap-path-prefix=$HOME=~" \
     CC_x86_64_linux_android="$NDK_BIN/x86_64-linux-android32-clang" \
     AR_x86_64_linux_android="$NDK_BIN/llvm-ar" \
     CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER="$NDK_BIN/x86_64-linux-android32-clang" \
     cargo build --locked --release --target x86_64-linux-android ) \
  || { echo "    FATAL: cargo build failed — refusing to package a stale native lib"; exit 1; }
[ -f "$JNILIB" ] || { echo "    FATAL: $JNILIB missing after a successful cargo build"; exit 1; }
# Verify the link actually resolved. A .so that cannot be dlopen'ed is worse than a missing one: it
# installs cleanly and fails only at the first control connection, in the truck.
if UND=$("$NDK_BIN/llvm-readelf" --dyn-syms "$JNILIB" 2>/dev/null | grep -c " UND eld_enc_"); then
    [ "$UND" -eq 0 ] || {
        echo "    FATAL: $UND unresolved eld_enc_* symbols in $JNILIB — the ELD shim was built for"
        echo "           the wrong architecture. Remove target/*/release/build/eld-codec-*/ and rerun;"
        echo "           the cc crate caches the bad object and will happily reuse it."
        exit 1
    }
fi
mkdir -p "$WORK/lib/x86_64"
cp "$JNILIB" "$WORK/lib/x86_64/"
echo "    packaging $(basename "$JNILIB") ($(stat -f%z "$JNILIB") bytes, sha $(shasum -a 256 "$JNILIB" | cut -c1-12)) for x86_64"

echo "[4/6] add classes.dex into apk"
cp "$WORK/base.apk" "$WORK/app-unsigned.apk"
( cd "$WORK/dex" && zip -q -X "$WORK/app-unsigned.apk" classes.dex )
# Native libs must be STORED, not deflated, so the loader can mmap them straight from the APK.
[ -d "$WORK/lib" ] && ( cd "$WORK" && zip -q -X -0 "$WORK/app-unsigned.apk" lib/x86_64/*.so )

echo "[5/6] zipalign"
"$BT/zipalign" -f -p 4 "$WORK/app-unsigned.apk" "$WORK/app-aligned.apk"

echo "[6/6] apksigner (debug key)"
"$BT/apksigner" sign \
    --ks "$HOME/.android/debug.keystore" \
    --ks-pass pass:android --key-pass pass:android \
    --ks-key-alias androiddebugkey \
    --out "$WORK/gmccpa-debug.apk" \
    "$WORK/app-aligned.apk"

# Verify to a file first: piping into head makes the pipeline exit status head's, so a FAILED
# verification would look like a successful build.
"$BT/apksigner" verify --print-certs "$WORK/gmccpa-debug.apk" > "$WORK/verify.txt"
head -3 "$WORK/verify.txt"

# Stamp the artifact with the git sha so a new build can never clobber the frozen golden APK
# Artifacts are named for the app (gmccpa-debug-*), renamed from netprobe-debug-* 2026-09-10: the
# app stopped being "NetProbe" in 2026-08 and is labelled "GM CCPA" (installs as $INSTALL_PKG). The FROZEN GOLDEN
# build keeps its historical name, apk/netprobe-debug-v4.0.apk = baseline-2026-08-05-working — it is
# a real file in the standalone archive and must not be renamed. versionCode was held constant at 7
# across hardening so any of these installed `-r` over another without a downgrade rejection
# (preserving carplay_peers.bin); since 2026-09-11 it is the commit count instead, so a rollback to
# one of those older artifacts needs `adb uninstall -k <its package>` first (a different package name
# is a separate app to the installer, not an upgrade — see docs/06 §4).
#
# NO "latest" SYMLINK (removed 2026-09-10, owner's call). There used to be a gmccpa-debug-latest.apk
# convenience link and the docs told you to always install it. It is a footgun on exactly the rig
# this project runs on: a symlink survives a FAILED build pointing at the last SUCCESSFUL one, and
# versionCode was pinned at 7, so `adb install -r` accepted the stale artifact without a downgrade
# rejection and the truck silently ran code you did not just build. The commit-count versionCode now
# rejects that case on its own, but the SHA in the filename is still what ties a binary to its
# sources; installing by name forces you to look at it.
REPO="$GM_ROOT"
# A dirty tree produces a binary the SHA does not describe. Without a per-tree suffix two different
# artifacts land on the same filename and one silently overwrites the other — after which nothing on
# the device, in logcat or in the file tree says which sources an APK came from, and a bisect over
# apk/*.apk compares the wrong binaries. A dirty build shares its commit count with the clean commit
# it sits on, so among those STAMP's dirty-tree hash is the ONLY discriminator. The name comes from
# STAMP (computed once, above) — this used to re-derive the sha and `-dirty` with a second, weaker
# predicate of its own, which could disagree with the asset and the versionName.
# apk/ is BUILD OUTPUT and is not tracked — the historical sha-stamped builds and the golden APK
# stayed behind in the standalone gm_ccpa checkout when this project moved under ccpa_custom/host.
# Create it on demand so a fresh clone builds without a manual step.
mkdir -p "$REPO/apk"
OUT="$REPO/apk/gmccpa-debug-$STAMP$FLAVOR_SFX.apk"
cp "$WORK/gmccpa-debug.apk" "$OUT"
echo "[done] $OUT ($(stat -f%z "$OUT") bytes)"
echo "[install] package $INSTALL_PKG (launch: am start -n $INSTALL_PKG/zeno.gmccpa.MainActivity)"
echo "[install] adb install -i com.android.vending -r -g --user 10 $OUT"
echo "WORKDIR=$WORK"
