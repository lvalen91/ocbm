#!/bin/bash
# UPX-pack box binaries for deployment — correctly, i.e. with UPX 3.96 inside the Lima `ccpa-build`
# VM, never the host's UPX.
#
# WHY THIS EXISTS: docs/ops/00_BUILD_AND_DEPLOY.md records that packing is a manual step and that the HOST UPX (5.x) writes a
# stub that SEGFAULTS on the box's 3.14 kernel. That knowledge lived only in prose, so every session
# either re-derived it or shipped an unpacked binary. This makes the correct path one command:
#
#   tools/upx_pack.sh target/armv7-unknown-linux-musleabihf/release/ocbmd [more...]
#
# Prints each packed artifact's path on stdout (staged under /tmp/upxout/), so it composes with
# ocbm_push.sh:
#
#   P=$(tools/upx_pack.sh target/armv7-unknown-linux-musleabihf/release/ocbmd)
#   tools/ocbm_push.sh "$P" /usr/sbin/ocbmd 755
#
# Compile-time optimisation is NOT this script's job and is already in the tree: the root Cargo.toml
# ships opt-level="z" + lto + panic=abort + codegen-units=1 + strip for the box profile (with
# opt-level=2 overrides on the per-frame crypto/framing crates), and .cargo/config.toml pins
# target-cpu=cortex-a7. Build with `--release --target armv7-unknown-linux-musleabihf` and this packs
# what that produced.
set -euo pipefail

VM=${CCPA_BUILD_VM:-ccpa-build}
UPX_IN_VM=${UPX_IN_VM:-/tmp/upx396/upx-3.96-arm64_linux/upx}
OUTDIR=${OUTDIR:-/tmp/upxout}

[ $# -ge 1 ] || { echo "usage: $0 <armv7-binary> [...]" >&2; exit 2; }

if ! limactl list "$VM" 2>/dev/null | grep -q Running; then
    echo "[upx] starting Lima VM $VM (host UPX 5.x is NOT usable — its stub segfaults the box)" >&2
    limactl start "$VM" >&2
    # Bounded: an unbounded `until` loop turns a VM that never comes up (corrupt image, host out of
    # resources) into a build that hangs silently instead of failing.
    waited=0
    until limactl shell "$VM" true 2>/dev/null; do
        sleep 3; waited=$((waited + 3))
        [ "$waited" -ge 120 ] && { echo "[upx] FAIL: $VM did not become reachable in ${waited}s" >&2; exit 1; }
    done
fi

limactl shell "$VM" test -x "$UPX_IN_VM" || {
    echo "[upx] FAIL: no UPX 3.96 at $UPX_IN_VM in $VM (set UPX_IN_VM)" >&2; exit 1; }

mkdir -p "$OUTDIR"
for f in "$@"; do
    [ -f "$f" ] || { echo "[upx] FAIL: no such file: $f" >&2; exit 1; }
    name=$(basename "$f")
    limactl copy "$f" "$VM:/tmp/$name.upxin" >&2
    # -o refuses to overwrite, so clear any artifact from a previous run first.
    limactl shell "$VM" sh -c "rm -f /tmp/$name.upxout; $UPX_IN_VM --best -o /tmp/$name.upxout /tmp/$name.upxin" >&2
    # `upx -t` unpacks and checksums the payload: the only check that the 3.96 stub + this binary
    # actually round-trip. Never push an unverified packed binary — a bad one bricks the boot path.
    limactl shell "$VM" "$UPX_IN_VM" -t "/tmp/$name.upxout" >&2
    want=$(limactl shell "$VM" md5sum "/tmp/$name.upxout" | cut -d' ' -f1)
    limactl copy "$VM:/tmp/$name.upxout" "$OUTDIR/$name" >&2
    # Verify the artifact that actually LEAVES here, not just the one that was tested inside the VM —
    # `upx -t` ran pre-copy, so a truncated transfer would otherwise ship a binary nothing checked.
    got=$(md5 -q "$OUTDIR/$name")
    [ "$want" = "$got" ] || { echo "[upx] FAIL: $name md5 mismatch after copy (vm=$want host=$got)" >&2; exit 1; }
    printf '%s\n' "$OUTDIR/$name"
done
