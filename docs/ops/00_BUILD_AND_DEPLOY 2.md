# Build, footprint and deployment

> **STATUS:** CURRENT · single owner for this topic. Consolidated 2026-08-31 from pre-consolidation docs 05; the originals are in git history and in the 2026-08-31 backup. Correct this file in place — do not add a sibling.

How the box binaries are built, what they cost in flash/RAM, and how they are pushed.

## Build footprint

<!-- absorbed: ../ops/00_BUILD_AND_DEPLOY.md -->

### Toolchain

- **Box (armv7)** — target `armv7-unknown-linux-musleabihf`, **static (musl)**. The Rust daemons
  cross-build with `cargo zigbuild --release --target armv7-unknown-linux-musleabihf` (zig serves as
  the C cross-linker); `airplayd` additionally needs `FDK_AAC_PREFIX=$PWD/scratchpad/fdk/install` for
  its eld-codec. C probes use `zig cc -target arm-linux-musleabihf -static -Os -s`. Size profile below.
- **Host** — macOS/Linux native, or Android. The two host *apps* are
  `host/CarPlayHost/carlink_macOS` (Swift/Xcode, shipping) and `host/CarlinkAndroid` (Kotlin/Gradle,
  AAOS 12L / API 32 — **no NDK and no Rust inside the app**; in-tree on a feature branch, not yet
  merged to `main`). `host/ocbm-host` is native **Rust** (`rusb`), and "`ocbm-rescue`" is a *role* of
  that same binary (`ocbm-host console`), not a separate build. The only `clang` + `libusb` artifact in
  the tree is `host/accbench.c` (hand-built, see `host/README.md`). The `aarch64-linux-android` NDK
  target IS used — but for the AAOS/Pi port of the **box** daemons (`pi/`), not for any host client.

Rust size profile (`Cargo.toml`) — size-first everywhere **except** the per-frame hot paths:
```toml
[profile.release]
opt-level = "z"      # optimize for size
lto = true
panic = "abort"      # no unwinding
codegen-units = 1
strip = true

## Five per-package overrides opt OUT of size-first because they sit on the per-frame
## critical path (ChaCha20-Poly1305 on every 4K video + audio frame, ocbm-proto's
## Reassembler, receiver's A/V forwarding). Paired with target-cpu=cortex-a7 in
## .cargo/config.toml. NOTE this is SCALAR: the armv7 musl target spec carries an
## explicit -neon that target-cpu does not override, so nothing we ship is vectorized.
[profile.release.package.chacha20]
opt-level = 2
[profile.release.package.poly1305]
opt-level = 2
[profile.release.package.chacha20poly1305]
opt-level = 2
[profile.release.package.ocbm-proto]
opt-level = 2
[profile.release.package.receiver]
opt-level = 2
```
The size cost of those five lands almost entirely in `airplayd` — the only binary carrying crypto
**and** full A/V forwarding. `Cargo.toml` is the authority; this is a quote of it.

### Storage budget (the box has very little)

- Flash: **16 MB** SPI NOR — `mtd0` uboot 256 K / `mtd1` kernel 3328 K / `mtd2` **rootfs 12800 K
  (jffs2)**. Partition sizes are fixed by the vendor kernel/U-Boot.
- Live free after stripping riddleBox: **~6 MB** on the jffs2 rootfs. jffs2 **compresses on write**,
  so compressible binaries consume less on-media than their raw size.
- RAM: **128 MB**, ~107 MB free (tmpfs) — a real staging option.

### Component footprint

*(The pre-build estimate table that stood here is dropped: its three C rows described helpers that
were never written — every box daemon is Rust — and MEASURED figures follow below.)*

### Fitting the heavy case (stackable)

1. **Rust size profile** (above) — often 2–4× smaller than default release.
2. **UPX** — an *optional manual* shrink, NOT applied by the installer (**3.96**, run in the Lima `ccpa-build` VM; host
   UPX 5.x segfaults the box's 3.14 kernel, so `upx -t`-verify the packed output); ~50% further shrink.
   **`tools/upx_pack.sh <binary>...` is that procedure as one command** (starts the VM if needed, packs
   with 3.96, `upx -t`-verifies, prints the packed path under `/tmp/upxout/` for `ocbm_push.sh`) — added
   2026-08-25 because the "correct" path existed only as prose and sessions kept shipping unpacked
   binaries. Measured on the current pair: ocbmd 454,864 → 230,316 B, aa-bridge 399,568 → 201,952 B.
3. **jffs2 auto-compression** — free on-media reduction.
4. **Run from `/tmp` (RAM)** — persist a small `.tar.xz` of binaries in rootfs, unpack to the 107 MB
   tmpfs at boot via a tiny loader; running binaries never touch flash.
5. **Lean custom `mtd2`** — reflash a rootfs that drops more vendor cruft.

### What the size plan got right, and what it did not

The original recommendation was "keep the low-level glue in C, write OCBM in Rust". **The C half was
not taken** — every box daemon is Rust — and the "hard measured number" it deferred was produced (see
the measured blocks above). What held: the Rust size profile, UPX packing, and the box's scope —
pairing + key-derivation + the RTSP relay, with no decode on the box.
