#!/usr/bin/env bash
# c2air_ocbm_install.sh — install/trial OCBM on the C2Air (Allwinner V821, riscv32) over ADB.
#
# This is a SEPARATE script from tools/ocbm_install.sh, not a --variant of it. The two boxes differ
# in the three things that script is built around — the control channel, the gadget model, and what
# "revert" costs — so sharing one flow would mean a conditional at every step. See docs comparison
# at the bottom of this header.
#
# ============================== WHY A SEPARATE SCRIPT ==================================
#                     CCPA (A15W, i.MX6UL, armv7)        C2Air (V821, riscv32)
#   control channel   USB-NCM: telnet/ssh @192.168.50.2   ADB over functionfs (ffs.adb)
#   gadget model      ci_hdrc.1, SINGLE-function:         configfs composite `g1`:
#                     functions=ncm XOR accessory         acm.0 + ffs.adb, accessory ADDABLE
#   switch mechanism  /script/ncm_only flag + reboot      configfs symlink + UDC rebind, RUNTIME
#   rootfs            writable — changes persist          READ-ONLY squashfs
#   cost of revert    a boot + a flag file to restore     free: reboot reverts EVERYTHING
#   failsafe          /script/ocbm_failover watchdog      inherent (see below)
#
# ============================== THE SAFETY PROPERTY THIS RELIES ON =====================
# On the C2Air the gadget is built from scratch at every boot by /etc/init.d/rc.preboot, and the
# rootfs is a read-only squashfs. Everything this script touches is therefore VOLATILE:
#
#   * the gadget lives in configfs (tmpfs-backed)
#   * the binaries live in /tmp (tmpfs, 28 MB free)
#
# So a power cycle — or any reboot — is a COMPLETE revert to a known-good ADB box, in ~4.0 s from
# power-on. There is no persistent state to undo and no flag file to restore. That is a strictly
# stronger position than the CCPA's, and it is why `trial` below can afford to be bold: the worst
# case is "unplug it and plug it back in".
#
# Nothing here writes to flash. Making OCBM the boot default on this box is NOT a file copy — the
# rootfs is squashfs, so it needs a rebuilt image flashed to mtd2, which is deliberately OUT OF
# SCOPE for this script. `finalize` is therefore absent by design; see `docs` verb.
#
# ============================== HAZARDS SPECIFIC TO THIS BOX ===========================
#  * THE WATCHDOG RESETS THE BOX AT ~430 s FROM EVERY BOOT, AND rc.preboot's FEEDER IS THE CAUSE.
#    Measured five times — 427 s, 430 s, ~431 s with the full stack, ~431 s on a BARE IDLE BOX, and
#    once predicted in advance to the second. Anchored to BOOT, independent of workload.
#    NOT starvation: the feeder ran at a perfect 20.008 s cadence from 3.4 s to 423.58 s, i.e. it fed
#    4-7 s before the reset and never missed. The stock vendor rootfs never opens /dev/watchdog at
#    all; the feeder is this project's own addition. Best fit: the first open ARMS the sunxi hardware
#    watchdog, the pings never restart the silicon counter, and the real period is ~427 s, not the
#    300 s printed. Kill the feeder subshell, then `echo V > /dev/watchdog` (nowayout=0 permits the
#    magic close) — c2air_stack.sh does this first. Permanent fix: drop the feeder from rc.preboot in
#    the next squashfs build.
#    CORRECTION: this file previously said the kernel `[watchdogd]` keeps petting the device so
#    killing the feeder could not reset the board, and to leave it alone. That was inverted —
#    "watchdog did not stop!" means it STAYED ARMED after close — and following it left the ~430 s
#    reset in place across five debugging sessions.
#  * SINGLE HART. One CPU. The in-kernel AIC8800 firmware load already saturates it 1.9–2.4 s.
#  * WAIT FOR CONFIGURED, NOT FOR THE DEVICE NODE. Inherited verbatim from ocbm_install.sh and it
#    bit again here: /dev/usb_accessory appears as soon as the FUNCTION is created — before it is
#    linked into a config and long before a host enumerates. ocbmd started there takes
#    POLLHUP/POLLERR and exits for respawn. Poll /sys/class/android_usb/android0/state instead.
#  * UDISK IS NEARLY FULL and holds the per-unit MFi key. 316 KB free of 512 KB. Never stage here.
#    /tmp is the staging area; carplay.key is read in place.
#
# ============================== USAGE ==================================================
#   tools/c2air_ocbm_install.sh preflight        # read-only: identity, deps, space, gadget state
#   tools/c2air_ocbm_install.sh build            # cross-build riscv32 binaries on the host
#   tools/c2air_ocbm_install.sh push             # adb push to /tmp + md5 verify
#   tools/c2air_ocbm_install.sh trial [SECS]     # temporary accessory switch, self-resets (def 120)
#   tools/c2air_ocbm_install.sh postmortem       # read the trial's aftermath once ADB is back
#   tools/c2air_ocbm_install.sh revert           # reboot = full revert
#   tools/c2air_ocbm_install.sh docs             # what a persistent install would require
#
# `trial` TAKES ADB AWAY on purpose and relies on the dead-man to give it back. That is inherent:
# AOA needs a single-function accessory gadget, so ffs.adb cannot be in the config at the same time.
set -uo pipefail

SERIAL="${C2AIR_SERIAL:-c2air-debug-0002}"
TARGET=riscv32gc-unknown-linux-musl
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
GADGET=/sys/kernel/config/usb_gadget/g1
UDC=44100000.udc-controller
STATE=/sys/class/android_usb/android0/state
# The C2Air keeps its OWN ids; ocbm-host takes vid/pid as positional args, so there is no reason to
# rewrite idVendor/idProduct and risk the ADB identity. 0x1314/0x2d00 is the CCPA's, not a protocol
# constant.
VID=1f3a
PID=ace2

A() { adb -s "$SERIAL" "$@"; }
# NOTE: no `2>/dev/null` INSIDE the quoted shell command — on this vendor adbd a redirect inside the
# command string silently yields empty output. Redirect on the host side instead.
ash() { adb -s "$SERIAL" shell "$*"; }
say() { printf '\n\033[1m== %s\033[0m\n' "$*"; }
die() { printf '\033[31mFATAL: %s\033[0m\n' "$*" >&2; exit 1; }

need_dev() {
  A get-state >/dev/null 2>&1 || die "no ADB device '$SERIAL' (power-cycle the box; ADB is up ~4 s after power-on)"
}

# ---------------------------------------------------------------------------------------
preflight() {
  need_dev
  say "identity"
  ash 'uname -a; echo "isa: $(grep ^isa /proc/cpuinfo)"; echo "uptime: $(cut -d" " -f1 /proc/uptime)s"'
  say "OCBM dependencies"
  ash '
    for n in /dev/i2c-1 /dev/watchdog /mnt/UDISK/carplay.key; do
      [ -e "$n" ] && echo "  ok      $n" || echo "  MISSING $n"
    done
    [ -e /sys/class/bluetooth/hci0 ] && echo "  ok      hci0 (attached)" || echo "  absent  hci0 (expected: OCBM owns the attach)"
    [ -e /sys/class/net/wlan0 ]      && echo "  ok      wlan0" || echo "  MISSING wlan0"
    echo "  i2c bus: $(ls /dev/i2c-* | tr "\n" " ")"
  '
  say "space (staging goes to /tmp — UDISK is nearly full and holds the MFi key)"
  ash 'df -h /tmp /mnt/UDISK /'
  say "gadget"
  ash "echo 'functions: '\$(ls $GADGET/functions/ | tr '\n' ' ')
       echo 'config c.1: '\$(ls $GADGET/configs/c.1/ | tr '\n' ' ')
       echo 'UDC: '\$(cat $GADGET/UDC)
       echo 'state: '\$(cat $STATE)
       echo 'accessory node: '\$([ -e /dev/usb_accessory ] && echo present || echo absent)"
  say "watchdog feeder (must stay alive)"
  ash 'ps | grep -c "[w]atchdog" >/dev/null; ps | grep "[s]leep 20" || echo "  (feeder subshell not visible in ps — it spends most of its life in sleep)"'
}

# ---------------------------------------------------------------------------------------
# Cross-build. Rust has NO prebuilt std for riscv32: riscv32gc-unknown-linux-musl is a Tier 3
# target, so `rustup target add` fails on both stable and nightly. It must be built from source
# with -Z build-std, which needs nightly + the rust-src component. The LINKER is zig (0.16 —
# 0.13 could not build the musl CRT for riscv32); the vendor GCC 10.4.0 is not needed for these
# crates because none of them have C dependencies.
#
# Homebrew's cargo shadows rustup's on PATH and does not understand `+nightly`, hence `rustup run`.
build() {
  command -v zig >/dev/null || die "zig not found (brew install zig) — it is the riscv32 linker"
  rustup run nightly rustc -vV >/dev/null 2>&1 || die "nightly toolchain missing (rustup toolchain install nightly)"
  rustup component list --toolchain nightly 2>/dev/null | grep -q 'rust-src (installed)' \
    || die "rust-src missing (rustup component add rust-src --toolchain nightly)"

  local wrap="$REPO/target/zigcc-rv32"
  mkdir -p "$(dirname "$wrap")"
  cat > "$wrap" <<'EOF'
#!/bin/sh
exec zig cc -target riscv32-linux-musl \
  -Wno-unused-command-line-argument -Wno-ignored-optimization-argument "$@"
EOF
  chmod +x "$wrap"

  export CARGO_TARGET_RISCV32GC_UNKNOWN_LINUX_MUSL_LINKER="$wrap"
  export CC_riscv32gc_unknown_linux_musl="$wrap"

  # The whole box-side set builds for riscv32 from the SHARED sources. ocbmd needed no changes at
  # all; iap2d/airplayd needed only the shared `portable-atomic` fix (rv32 has no 64-bit atomics, so
  # core provides no AtomicU64/AtomicI64) — that fix lives in the vendored crates, NOT here, because
  # it is an arch fix rather than a C2Air one and armv7 must keep working. See c2air/README.md.
  #
  # airplayd is built --no-default-features: `mic-uplink-eld` statically links a cross-built
  # libfdk-aac and needs FDK_AAC_PREFIX for the SAME ABI, and no riscv32 prefix exists yet. The cost
  # is the wireless AAC-ELD mic uplink.
  # carplay-wireless + rx-connect are the WIRELESS CarPlay pair (BT bring-up/SSP/SDP/RFCOMM + WiFi
  # handoff, and the mDNS advertiser). They build for riscv32 unchanged. Note that carplay-wireless
  # needs CARPLAY_HCI_BACKEND=native at RUNTIME on this box — see `docs`.
  say "building the riscv32 set (compiles std from source — ~20 s warm, minutes cold)"
  local p
  for p in ocbmd iap2d carplay-wireless rx-connect c2air-btattach; do
    rustup run nightly cargo build -p "$p" --release --target "$TARGET" \
      -Z build-std=std,panic_abort || die "build failed: $p"
  done
  rustup run nightly cargo build -p airplayd --release --target "$TARGET" \
    -Z build-std=std,panic_abort --no-default-features || die "build failed: airplayd"

  local total=0 b sz
  for p in ocbmd iap2d airplayd carplay-wireless rx-connect c2air-btattach; do
    b="$REPO/target/$TARGET/release/$p"
    [ -f "$b" ] || die "missing artifact $b"
    file "$b" | grep -q 'UCB RISC-V' || die "wrong arch for $p: $(file "$b")"
    # Board is rv32imafdc = HARD-FLOAT (ilp32d). A soft-float binary loads and THEN traps, so assert
    # the ABI rather than trusting the target triple.
    file "$b" | grep -q 'double-float ABI' || die "wrong float ABI for $p (board is ilp32d): $(file "$b")"
    sz=$(stat -f%z "$b"); total=$((total + sz))
    printf '  ok  %-16s %8d B\n' "$p" "$sz"
  done
  # For context on a persistent install: the baseline squashfs leaves 9,416 K free in the 11,456 K
  # mtd2 slot, so the whole set fits with room to spare.
  printf '  -- total %d B (%d KB) vs 9,416 KB free in the rootfs slot\n' "$total" "$((total / 1024))"
}

# ---------------------------------------------------------------------------------------
push() {
  need_dev
  local b="$REPO/target/$TARGET/release/ocbmd"
  [ -f "$b" ] || die "no binary — run 'build' first"
  say "pushing to /tmp (tmpfs — deliberately volatile)"
  A push "$b" /tmp/ocbmd || die "push failed"
  ash 'chmod 755 /tmp/ocbmd'
  local want have
  want=$(md5 -q "$b")
  have=$(ash 'busybox md5sum /tmp/ocbmd' | awk '{print $1}')
  [ "$want" = "$have" ] || die "md5 mismatch: host=$want box=$have"
  printf '  ok  md5 %s verified on box\n' "$want"
}

# ---------------------------------------------------------------------------------------
# THE FAILSAFE — read this before trusting the dead-man, because the dead-man is the WEAKER of the
# two and the ordering here was learned by getting it wrong twice on 2026-08-17.
#
# PRIMARY FAILSAFE: OCBM ITSELF. Once ocbmd is up, caps=0x3f includes CH_CONSOLE, which is a ROOT
# SHELL over the accessory link. So the box is never actually unreachable while ocbmd lives — you
# do not need ADB to get back:
#
#     printf 'sync; /sbin/reboot -f\n' | ocbm-host console 1f3a ace2
#
# That is how the 2026-08-17 trial was recovered, and it is strictly better than a timer: it works
# on demand, needs no prediction of how long you will want, and proves the console channel as a
# side effect. The dead-man only has to cover the window where ocbmd never comes up at all.
#
# SECONDARY FAILSAFE: the timer below. It works, but NOT RELIABLY, and the discrepancy is unexplained:
#   * In ISOLATION it fired exactly as designed — armed at uptime 64.07 with a 45 s timer, marker
#     written at 109.07 (+45 s to the centisecond), box reset, ADB back 4 s later. The marker went to
#     UDISK so the evidence survived the reboot it caused.
#   * DURING THE TRIAL it did NOT fire. The box sat in accessory mode well past its 150 s deadline
#     and had to be rebooted over the OCBM console instead. Cause not established — candidates are
#     `reboot -f` blocking on USB gadget teardown while the accessory function is bound, or the
#     process being reaped during the config rewrite. DO NOT rely on this alone.
#
# It does survive the adb session closing, which is worth stating because the CCPA notes record the
# OPPOSITE for ssh/telnet ("anything backgrounded dies of SIGHUP; setsid did not help"). On this box
# `setsid` + a script file + all three fds redirected survives — verified by closing the shell and
# finding the pid alive. So SIGHUP is not the explanation for the trial failure.
#
# TERTIARY: unplug/replug. Always works — read-only squashfs means nothing persisted.
#
# The hardware watchdog is deliberately NOT used; see HAZARDS above for why it cannot work.
arm_deadman() {
  local secs="$1"
  ash "cat > /tmp/deadman.sh <<EOF
#!/bin/sh
sleep $secs
/sbin/reboot -f
EOF
chmod 755 /tmp/deadman.sh
setsid /tmp/deadman.sh < /dev/null > /tmp/deadman.log 2>&1 &
echo \"deadman armed: force-reboot in $secs s unless disarmed (uptime now \$(cut -d' ' -f1 /proc/uptime))\""
  # Prove it is actually detached and alive, rather than trusting that it is.
  ash 'busybox pidof -s dm 2>/dev/null; ps | grep "[d]eadman.sh" >/dev/null && echo "  confirmed: deadman survived session close" || echo "  WARNING: deadman not visible in ps"'
}

disarm_deadman() { ash 'busybox pkill -f deadman.sh; echo "deadman disarmed"'; }

# ---------------------------------------------------------------------------------------
# THE MODE SWITCH — and the correction that cost a lockout on 2026-08-17.
#
# The C2Air gadget is a configfs COMPOSITE (c.1 = acm.0 + ffs.adb), so the first attempt tried to
# make OCBM and ADB COEXIST by linking accessory.gs0 into the live c.1 alongside ffs.adb and
# rebinding. THAT DOES NOT WORK: the box fell off the USB bus entirely — not merely off ADB, it
# vanished from ioreg — and needed a dead-man reset to come back.
#
# The lesson is that ocbm_install.sh's model was right all along and the composite gadget is a red
# herring. AOA wants a SINGLE-FUNCTION accessory gadget, exactly like `functions=accessory` on the
# CCPA's ci_hdrc.1. So this REPLACES the config contents rather than adding to it:
#
#     unbind UDC -> unlink acm.0 + ffs.adb -> link accessory.gs0 -> rebind UDC
#
# ADB necessarily goes away for the duration. That is inherent to the transport, not a bug, and it
# is why the dead-man is armed FIRST and is non-optional.
switch_to_accessory() {
  local secs="$1"
  need_dev
  say "BEFORE"
  ash "echo 'c.1: '\$(ls $GADGET/configs/c.1/ | tr '\n' ' '); echo 'state: '\$(cat $STATE)"

  say "arming dead-man (${secs}s) — ADB WILL drop; this is what brings it back"
  arm_deadman "$secs"

  say "switching gadget to single-function accessory"
  # One shell: once ffs.adb is unlinked the connection is going away, so every step has to already
  # be queued on the box. `echo '' > UDC` has returned -19 here; tolerated, the rebind is what counts.
  # The class triple is zeroed while the UDC is unbound. rc.preboot sets 0xEF/0x02/0x01
  # (Miscellaneous/Common/IAD = "this config contains Interface Association Descriptors") for the
  # acm+adb composite. After the mutation the config holds ONE vendor-FF interface and no IAD, so the
  # device descriptor is lying — a real USB-IF violation. ocbm_boot.sh:5-7 builds the CCPA accessory as
  # a PURE device (class 0) and warns macOS seizes an IAD composite.
  # HONEST SCOPE: this is descriptor hygiene and parity, NOT a fix for the A/V stall — macOS
  # demonstrably enumerated, claimed the interface and ran HELLO/MFi/console over the 0xEF descriptor,
  # and bDeviceClass only affects driver matching at enumeration. It removes a confounder (and Windows
  # usbccgp genuinely does misbehave on EF/02/01-without-IAD).
  ash "mkdir -p $GADGET/functions/accessory.gs0
       echo '' > $GADGET/UDC
       echo 0 > $GADGET/bDeviceClass
       echo 0 > $GADGET/bDeviceSubClass
       echo 0 > $GADGET/bDeviceProtocol
       rm -f $GADGET/configs/c.1/acm.0 $GADGET/configs/c.1/ffs.adb
       ln -sf $GADGET/functions/accessory.gs0 $GADGET/configs/c.1/accessory.gs0
       echo $UDC > $GADGET/UDC
       sync" >/dev/null 2>&1 || true
  echo "  switch issued (ADB is expected to be gone now)"
}

# Watch the HOST's USB bus for the accessory device. ADB cannot tell us anything at this point, so
# ioreg is the only channel. The C2Air keeps its own 1f3a:ace2 ids.
wait_for_accessory() {
  local i
  say "watching host USB for the accessory (up to 25 s)"
  for i in $(seq 1 25); do
    sleep 1
    if ioreg -p IOUSB -l -w 0 2>/dev/null | grep -qi 'c2air\|accessory.gs0'; then
      printf '  device present on the bus after %ss\n' "$i"
      ioreg -p IOUSB -l -w 0 2>/dev/null | grep -iE '"USB Product Name"|"idVendor"|"idProduct"' | grep -iA2 -B1 c2air | head -6
      return 0
    fi
  done
  echo "  NOT on the bus after 25 s — the dead-man will reset the box"
  return 1
}

# ---------------------------------------------------------------------------------------
# The ORDER here is the one thing that matters, and it is the inverse of the obvious one.
#
# ocbmd must be started BEFORE the gadget switch, not after — because after the switch there is no
# ADB left to start it with. It is launched detached with the accessory function already
# instantiated (so /dev/usb_accessory exists) but not yet bound to a host.
#
# That collides with the trap inherited from ocbm_install.sh: /dev/usb_accessory appears as soon as
# the FUNCTION exists, long before a host enumerates, and an ocbmd started there takes
# POLLHUP/POLLERR and exits "for respawn". That is what happened on the first smoke test here
# (2026-08-17): ocbmd was started with accessory.gs0 created but unlinked and no host, and the box
# rebooted ~1 min later.
#
# So ocbmd needs a RESPAWNER, exactly as `::respawn:/script/run_ocbmd.sh` provides on the CCPA. This
# box has no inittab entry for it, so the trial supplies one as a detached loop: ocbmd exits on
# POLLHUP, the loop restarts it, and whichever attempt coincides with the host's enumeration is the
# one that sticks. Without this the trial is a race it usually loses.
trial() {
  need_dev
  local secs="${1:-120}"
  [ -f "$REPO/target/$TARGET/release/ocbmd" ] || die "run 'build' && 'push' first"
  ash '[ -x /tmp/ocbmd ] && echo ok || echo MISSING' | grep -q MISSING && die "ocbmd not on box — run 'push'"

  say "instantiating accessory function (creates /dev/usb_accessory; does NOT touch the UDC)"
  ash "mkdir -p $GADGET/functions/accessory.gs0; ls -l /dev/usb_accessory"

  say "starting ocbmd under a respawn loop, detached"
  # Respawner semantics ported from tools/run_ocbmd.sh + the CCPA's `::respawn:` inittab entry.
  # The first version of this was a 200-iteration loop, which is wrong twice over:
  #   * BOUNDED. ocbmd exits within milliseconds on a pre-enumeration POLLHUP, so 200 x 300 ms is
  #     only ~60-80 s of supervision; after that ocbmd is gone for good with nothing to restart it.
  #     inittab's `::respawn:` is unbounded, and run_ocbmd.sh:13 `exec`s under it.
  #   * NO SINGLE-INSTANCE GUARD. run_ocbmd.sh:11 waits while any ocbmd lives. Without that, re-running
  #     `trial` leaves a SECOND respawner: two ocbmd processes then race for /dev/usb_accessory, and —
  #     because ocbmd binds its five A/V listeners (9001-9005) with `filter_map(...ok())` and DROPS
  #     failures silently (ocbmd main.rs:2411-2425) — the loser comes up with a perfect control plane
  #     and ZERO A/V listeners. That presents exactly as "link fine, MFi fine, console fine, no video".
  # Also waits for /dev/usb_accessory like run_ocbmd.sh:10 rather than hot-cycling against its absence.
  ash 'busybox pkill -f ocbmd_respawn.sh 2>/dev/null
cat > /tmp/ocbmd_respawn.sh <<EOF
#!/bin/sh
n=0
while true; do
  # single-instance guard (run_ocbmd.sh:11): never run alongside a live ocbmd
  while ps | grep -v grep | grep -q "[/]tmp/ocbmd\$"; do sleep 3; done
  # wait for the accessory node (run_ocbmd.sh:10) instead of burning restarts before the gadget binds
  i=0
  while [ ! -e /dev/usb_accessory ] && [ \$i -lt 100 ]; do i=\$((i+1)); usleep 100000; done
  n=\$((n+1))
  echo "--- attempt \$n at uptime \$(cut -d" " -f1 /proc/uptime) ---" >> /tmp/ocbmd.log
  /tmp/ocbmd >> /tmp/ocbmd.log 2>&1
  usleep 300000
done
EOF
chmod 755 /tmp/ocbmd_respawn.sh
setsid /tmp/ocbmd_respawn.sh < /dev/null > /tmp/respawn.log 2>&1 &
usleep 500000
ps | grep "[o]cbmd_respawn" >/dev/null && echo "respawner up" || echo "FAILED to start respawner"'

  switch_to_accessory "$secs"

  if wait_for_accessory; then
    say "HOST SIDE — run this now, in another shell:"
    cat <<EOF
    cd $REPO
    cargo run --release -p ocbm-host -- hello $VID $PID

  vid/pid are POSITIONAL args and the C2Air keeps its own ids: pass $VID $PID,
  NOT the CCPA's 1314 2d00 (that default is the CCPA's gadget, not a protocol constant).
EOF
  fi

  say "RECOVERY — do not wait for the dead-man; it did not fire during the 2026-08-17 trial"
  cat <<EOF
  When you are done, put the box back on ADB yourself, over OCBM's own root console:

      printf 'sync; /sbin/reboot -f\\n' | ./target/release/ocbm-host console $VID $PID

  The dead-man (${secs}s) is only a backstop for "ocbmd never came up". If neither works,
  unplug and replug — read-only squashfs means nothing persisted.

  Then:  $0 postmortem
EOF
}

# Read the trial's own log after the box has come back. /tmp is tmpfs and the reset wipes it, so
# this only works if you copy the log out BEFORE the reset — which is what this does, from UDISK.
# Kept separate from `trial` because at trial time there is no ADB to read anything with.
postmortem() {
  need_dev
  say "uptime (small = we just came back from the trial reset)"
  ash 'cut -d" " -f1 /proc/uptime'
  say "gadget state (should be back to acm.0 + ffs.adb, rebuilt by rc.preboot)"
  ash "echo 'c.1: '\$(ls $GADGET/configs/c.1/ | tr '\n' ' '); echo 'state: '\$(cat $STATE)"
  say "trial logs — NOTE: /tmp is tmpfs and was wiped by the reset"
  ash 'ls -l /tmp/ocbmd.log /tmp/respawn.log 2>&1; cat /tmp/ocbmd.log 2>&1 | tail -40'
  echo
  echo "  If the logs are gone, that is expected. To capture across the reset, the respawn loop"
  echo "  must tee to /mnt/UDISK — but that partition has only ~316 KB free and holds carplay.key,"
  echo "  so cap it hard or capture from the host side (ocbm-host output) instead."
}

# ---------------------------------------------------------------------------------------
# Revert is a reboot. There is genuinely nothing else to undo: the rootfs is read-only squashfs,
# the gadget is rebuilt by rc.preboot, and /tmp is tmpfs.
revert() {
  need_dev
  ash 'busybox pkill -f deadman.sh; busybox pkill ocbmd; sync; /sbin/reboot' || true
  say "rebooting — ADB should return in ~4 s"
  local i
  for i in $(seq 1 30); do
    sleep 1
    A get-state >/dev/null 2>&1 && { printf '  ADB back after %ss\n' "$i"; ash 'cut -d" " -f1 /proc/uptime'; return 0; }
  done
  die "ADB did not return in 30 s — unplug and replug"
}

docs() {
  cat <<'EOF'
WHAT A PERSISTENT (BOOT-DEFAULT) OCBM INSTALL WOULD REQUIRE ON THE C2AIR
=======================================================================
Deliberately not implemented here. Unlike the CCPA — where OCBM install is a file copy onto a
writable rootfs plus a flag file — the C2Air rootfs is a READ-ONLY SQUASHFS. Persistence means:

  1. unsquashfs the baseline (flash_dumps/c2air_v821_2026-08-17_ocbm_baseline/partitions/
     rootfs_0x370000.bin), add ocbmd + an rc hook, re-mksquashfs (xz, 262144, DUPLICATES|EXPORTABLE)
  2. it must fit the 11,456 K mtd2 slot — the baseline leaves 9,416 K free, so there is room
  3. flash it. NOT with flashcp: you cannot overwrite the squashfs you are executing from, because
     its pages are read on demand. That means FEL (`xfel spinor write 0x370000 ...`) via the hidden
     button, or an SPI programmer.
  4. readback-verify (`xfel spinor read` + cmp). Large writes intermittently abort with
     "usb bulk send error"; retry the whole write.

Until then /tmp staging + `trial` is the whole story, and it is enough to prove the protocol.

BLOCKER FOR THE FULL STACK: rv32 HAS NO 64-BIT ATOMICS
======================================================
ocbmd builds clean (deps: ocbm-proto + libc only). iap2d and airplayd do not:
`std::sync::atomic::AtomicU64` does not exist on riscv32 — the ISA has no 64-bit LR/SC — and it is
used in 8 files / ~26 references:

    10  crates/vendor/receiver/src/session.rs      3  ccpa/mfid/src/main.rs
     3  crates/vendor/receiver/src/levers.rs       3  ccpa/airplayd/src/main.rs
     2  crates/vendor/receiver/src/uplink.rs       2  crates/vendor/receiver/src/datastream.rs
     2  crates/vendor/iap2-core/src/metadata.rs    1  crates/vendor/receiver/src/relay.rs

Recommended fix: the `portable-atomic` crate, which exists for exactly this case and provides a
lock-based AtomicU64 fallback on targets without native 64-bit atomics. It is a near drop-in
(`use portable_atomic::AtomicU64`). Do NOT narrow these to AtomicU32 wholesale: some are counters
where that is harmless, but receiver/session.rs uses 64-bit values for timestamps and sequence
numbers, where truncation is a real bug.

airplayd has a SECOND blocker beyond atomics: `mic-uplink-eld` statically links a cross-built
libfdk-aac and needs FDK_AAC_PREFIX for the SAME ABI. Only armv7 and android-x86_64 prefixes exist,
so a riscv32 fdk-aac must be cross-built or the feature dropped (--no-default-features).
EOF
}

case "${1:-}" in
  preflight) preflight ;;
  build)     build ;;
  push)      push ;;
  trial)      trial "${2:-120}" ;;
  postmortem) postmortem ;;
  revert)    revert ;;
  docs)      docs ;;
  # BSD sed (macOS) has no \? — use a bounded interval instead.
  *) sed -n '/^# ===* USAGE/,/^set -uo/p' "${BASH_SOURCE[0]}" | sed '1d;$d;s/^#[ ]\{0,1\}//' ;;
esac
