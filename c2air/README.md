# C2Air (Allwinner V821, riscv32) — OCBM port

Board-specific material for the **C2Air**, a second adapter unrelated to the CPC200-CCPA:
**Allwinner V821, 32-bit RISC-V** (`rv32imafdc`, `sun300i_riscv32`), kernel 5.4.220, musl, 58 MB RAM,
16 MiB SPI NOR, USB `1f3a:ace2`. Control channel is **ADB over functionfs**, not USB-NCM.

> [!IMPORTANT]
> Nothing here is cross-flashable with the CCPA. Different SoC, different instruction set.

## The headline: OCBM needs almost no C2Air-specific Rust

This was the open question, and it was answered by building and running it rather than by estimating.
**All three box daemons compile for riscv32 from the shared CCPA sources**, and `ocbmd` runs on the
hardware with every OCBM channel working:

| Daemon | C2Air-specific code required |
|---|---|
| `ocbmd` | **none** — builds byte-for-byte from the shared source |
| `iap2d` | **none** — only the shared `portable-atomic` fix (see below) |
| `airplayd` | **none** — shared fix + `--no-default-features` to drop the fdk-aac ELD uplink |

Verified live on hardware 2026-08-17 with `ocbm-host … 1f3a ace2`:

```
HELLO_ACK: box v1 caps=0x0000003f mode=0        <- identical caps to armv7
MFI cert: 608 bytes  first8=3082025c06092a86    <- genuine DER cert from the on-board coprocessor
MFI sign: 64-byte signature  first8=fdfc15130c5c5a11
[ocbmd] CONSOLE attached (root)
```

### Why the divergence is this small

Not by design — by a genuine alignment that could easily have gone the other way. The three hardcoded
board facts that normally break a port are **identical on both boards**:

| Fact | CCPA | C2Air |
|---|---|---|
| MFi coprocessor | `/dev/i2c-1 @0x11` | `/dev/i2c-1` (`LY_MFI_I2C_BUS=1`) — same |
| USB gadget state | `/sys/class/android_usb/android0/state` | present, same path, reports `CONFIGURED` |
| WLAN interface | `wlan0` | `wlan0` (AIC8800, driver in-kernel) |

And the Bluetooth UART difference — C2Air `/dev/ttyS1` @ 500000 H4 vs the CCPA's `ttymxc2` @ 115200
H5 — **costs no Rust at all**, because the Rust never touches the UART: it speaks `AF_BLUETOOTH`
sockets, and the line-discipline attach happens outside it.

So `docs/wireless/01_BT_AND_RADIO.md`'s warning against hardcoding a chipset turns out to be the thing that saved this port:
the daemons were already written against interfaces rather than against the CCPA's specific radios.

### The two real changes belong in SHARED code, not here

Both are **riscv32 fixes, not C2Air fixes**, so putting them in this folder would be wrong — they
benefit any 32-bit target and must not be forked:

1. **`portable-atomic`** — rv32 has no 64-bit LR/SC, so `core` provides no `AtomicU64`/`AtomicI64`.
   8 files, ~26 references (`receiver/session.rs` 10, `receiver/levers.rs` 3, `mfid` 3, `airplayd` 3,
   `receiver/uplink.rs` 2, `receiver/datastream.rs` 2, `iap2-core/metadata.rs` 2, `receiver/relay.rs`
   1). `portable_atomic::AtomicU64` is a drop-in with a `const` constructor, so the `static` uses keep
   working. **Deliberately NOT narrowed to `AtomicU32`**: some sites are throwaway counters where that
   would be harmless, but `receiver/session.rs` holds timestamps and sequence numbers where
   truncation is a real bug. **Verified no armv7 regression** — all 9 `build.sh` artifacts still build,
   `airplayd` included.
2. **`musl32_time64` — the one that actually mattered.** See the next section; it is a build-config
   change, not a code change, and without it the box crashes.

## What actually lives here

```
c2air/
  btattach/    the ONLY C2Air-specific Rust crate (lib + thin bin)
  tools/       c2air_ocbm_install.sh — preflight | build | push | trial | postmortem | revert | docs
  docs/        port notes
```

### `btattach` — and why it is legitimately board-specific

The C2Air ships **no Bluetooth userspace whatsoever**: the vendor's `btapp` lived in the `customer`
partition that the OCBM baseline absorbed, and it needs `liblylinkmw.so.1` so it cannot run
standalone. The kernel side is complete and the AIC8800 firmware patch loads **in-kernel**, so
`/lib/firmware` is empty and unnecessary. Only the line-discipline attach was missing.

On the CCPA that step is performed by each unit's **own** vendor helper, dispatched by
`init_bluetooth_wifi.sh` — and per `docs/wireless/01_BT_AND_RADIO.md` that must never be replaced by a chipset-specific
reimplementation, because only one chip's driver set is present in any given rootfs. **The C2Air has
no such helper to dispatch.** Writing one here is therefore installing a bring-up path where the
vendor provides none, confined to a crate no CCPA build links — not a doctrine violation.

It is a **library** first, because the attach is bound to the fd's lifetime: `HCIUARTSETPROTO` binds
the tty to the HCI stack for exactly as long as the descriptor lives. So whoever owns Bluetooth must
hold it, and on this box that is OCBM. Verified both directions on hardware:

```
[btattach] attached as hci0 — /dev/ttyS1 @500000 (fd 3)
1: uart:SUNXI mmio:0x42500400 irq:135 tx:424 rx:665 RTS|CTS|DTR
```

`tx:424 rx:665` is **byte-for-byte what the resources repo recorded for the reference C
`hciattach`** — independent corroboration that this reproduces it exactly. `RTS|CTS` confirms
hardware flow control, and the kernel agrees (`bt uart baud: 500000, flowctrl: 1`). Killing the
process removed `hci0`, proving the fd binding rather than assuming it.

Per `docs/wireless/01_BT_AND_RADIO.md`, note that `hci0` merely existing proves nothing — the moved tty counters are the
evidence, not the sysfs node.

Board facts come from the environment (`LY_BT_UART`, `LY_BT_BDRATE`, exported by the baseline's
`/etc/profile`), never from constants in the source. A non-login `adb shell` does not source the
profile, so the built-in defaults are a documented fallback rather than the primary path.

`sunxi:uart-ng-uart-ng1:[INFO]: uart1, baud 500000 beyond rance` in dmesg is **advisory** — the
resources repo established that `sunxi_uart_check_baudset()`'s return value is discarded by its
caller, so it gates nothing. The kernel applies 500000 regardless, as the counters show.

## Building

`riscv32gc-unknown-linux-musl` is a **Tier 3** target: `rustup target add` fails on stable *and*
nightly, so `std` must be built from source.

```sh
c2air/tools/c2air_ocbm_install.sh build     # ocbmd; asserts arch AND hard-float ABI
```

Under the hood, and the parts that are not obvious:

- `rustup run nightly cargo … -Z build-std=std,panic_abort` with the `rust-src` component. ~22 s warm.
- Linker is **zig** (0.16; 0.13 cannot build the riscv32 musl CRT).
- **Assert the float ABI, not just the triple.** The board is `rv32imafdc` = hard-float `ilp32d`; a
  soft-float binary loads and then traps.
- Homebrew's `cargo`/`rustc` shadow rustup's on `PATH` and do not understand `+nightly` — hence
  `rustup run`. The same shadowing makes a bare `cargo build --target armv7…` fail with "can't find
  crate for `std`" even though rustup has that target installed.
- No C dependencies in `ocbmd`/`btattach`, so the vendor GCC is not needed for them. For anything
  **with** C in it, prefer the vendor GCC 10.4.0 — per `build-rv32.sh`, a zig-built busybox passed
  every applet-presence check and then segfaulted in `awk`.
- `airplayd` with its default features needs a **riscv32 `libfdk-aac`** for `FDK_AAC_PREFIX`; only
  armv7 and android-x86_64 prefixes exist today. Until one is cross-built, use
  `--no-default-features`, which costs the wireless AAC-ELD mic uplink.

## `musl32_time64` — the single most important thing on this page

**riscv32 Linux is time64-only** and musl's riscv32 port has had a 64-bit `time_t` since it was
added. The `libc` crate does not know that: its build.rs force-enables `musl_v1_2_3` (hence
`musl32_time64`) only for loongarch64, hexagon, ohos and pauthtest. riscv32 is missing, so 32-bit
musl falls back to the pre-1.2 `time_t = c_long`. Measured on the box **without** the flag:

    time_t = 4 bytes,  timespec = 8 bytes,  timeval = 8 bytes      (kernel wants 8 / 16 / 16)

Everything that followed was one symptom of that single mismatch:

| Symptom | Real cause |
|---|---|
| `carplay-wireless` SIGSEGV at startup (`sepc: 0`, null jump) | first daemon to spawn a *sleeping* thread |
| `std::thread::sleep` panics | `nanosleep` returns EINVAL; std asserts the errno can only be EINTR |
| `ocbmd` CT_SETTIME rejected | `settimeofday`/`clock_settime` get a mis-sized struct → EINVAL |
| wireless socket timeouts wrong | every `SO_RCVTIMEO`/`SO_SNDTIMEO` passed a mis-sized `timeval` |

`ocbmd` survived only because it is a single-threaded poll loop that never sleeps on a timespec —
which is exactly why "ocbmd works" was misleading evidence that the toolchain was sound.

The fix is `.cargo/config.toml`'s `[target.riscv32gc-unknown-linux-musl] rustflags = ["--cfg",
"libc_unstable_musl_v1_2_3"]`, scoped to this target so armv7 keeps its genuinely 32-bit `time_t`.
**It requires libc >= 0.2.189** — the machinery does not exist in 0.2.186, which is what `Cargo.lock`
pinned, so the flag silently did nothing until the lockfile was bumped.

Two traps that hid it:

* a standalone probe crate resolved 0.2.189 and behaved correctly while the workspace did not — the
  difference was the **lockfile**, not the config, which made the config itself look broken;
* `rm -rf target/<triple>` does **not** invalidate the libc build script — build scripts run on the
  host, in `target/release/build/libc-*`. A stale one keeps emitting the old cfg set.

**Verify with the compiler, not by inspection.** Under `not(musl32_time64)`, `libc::time_t` is
`#[deprecated]`, so any build referencing it warns. armv7 *should* warn; riscv32 should not:

```sh
# armv7 -> 1 (32-bit time_t, correct for the CCPA);  riscv32 -> 0 (time64, correct for the C2Air)
grep -c 'deprecated type alias'
```

These types gain **private padding fields** under the cfg, so `libc::timeval { .. }` struct literals
no longer compile. Build them with `zeroed()` + field assignment — that form works on every target,
which is why the 10 sites in the wireless crate were converted outright rather than cfg'd apart.

Device-verified after the fix: `[ocbm] box clock set to 1787013468 (unix)`, and the box's own `date`
read back real wall time.

## Wireless CarPlay — no `hciconfig` needed, and no new binary

The C2Air has **no BlueZ userspace**, so the obvious worry is that `carplay-wireless` shells out to
`hciconfig` for class/name/EIR/scan. It does — but only on the BlueZ path. `crates/vendor/wireless/
src/hci.rs` already implements every one of those operations natively with ioctls + raw HCI command
packets (written for the Android/Pi port, where BlueZ is equally absent). Selecting it is one env
var:

```sh
CARPLAY_HCI_BACKEND=native
```

So nothing needs to be written or cross-compiled to control Bluetooth here. Device-verified
2026-08-17, with `c2air-btattach` holding `hci0`:

```
[hci] native backend selected — not using hciconfig
[wireless] discoverable as "CarLink-b486" -- waiting for pairing or preempt
[sdp] serving 'Wireless iAPv2' (handle 0x00010000, RFCOMM ch 1) on L2CAP PSM 0x0001
[ssp-agent] SET_POWERED=1 / SET_BONDABLE=1 / SET_CONNECTABLE=1 / SET_SSP=1 / SET_IO_CAPABILITY: ok
[ssp-agent] pairing mode: Just-Works (NoInputNoOutput) — auto-accept pairing agent running
```

ttyS1 counters moved 424/665 → **1371/1379**, i.e. the class/name/EIR/scan writes and the mgmt setup
really reached the chip. `killall: hcid/bluetoothDaemon/sdpd: no process killed` is expected and
harmless — `stop_conflicting_daemons()` runs before the backend check and there is no BlueZ here.

The other shell-outs the crate makes (`pkill`, `pgrep`, `killall`) are all present as busybox
applets. `/usr/bin/true` is absent but only used in a `#[cfg(test)]` unit test.

## macOS app

`host/CarPlayHost/carlink_macOS/USB/USBDeviceManager.swift` matches against a hardcoded
`kSupportedDevices` table, so the app would never claim this box until `1f3a:ace2` was added — done.
The C2Air deliberately keeps its own ids rather than borrowing the CCPA's `1314:2d00`: nothing in
OCBM requires a particular VID/PID (`ocbm-host` takes them as positional arguments), and reusing the
CCPA's would make the two boxes indistinguishable to that table.

## Operating the box

See the header of `tools/c2air_ocbm_install.sh` for the full rationale. The two things most likely to
cost someone a lockout:

**The accessory switch must REPLACE the gadget config, not extend it.** `rc.preboot` builds a
configfs composite (`g1` = `acm.0` + `ffs.adb`), which invites the conclusion that accessory can
coexist with ADB. **It cannot** — linking `accessory.gs0` into the live `c.1` alongside `ffs.adb` and
rebinding dropped the box off the USB bus entirely (gone from `ioreg`, not merely from ADB). AOA
wants a single-function accessory gadget, exactly like `functions=accessory` on the CCPA's
`ci_hdrc.1`. What works: `unbind UDC → rm c.1/{acm.0,ffs.adb} → ln accessory.gs0 → rebind`. ADB
necessarily goes away for the duration; that is the transport, not a bug.

**`ocbmd` needs a respawner and must be started before the switch.** `/dev/usb_accessory` appears as
soon as the *function* is instantiated, long before a host enumerates, so the first attempt reliably
takes `POLLHUP` and exits — the "wait for CONFIGURED, not the device node" trap from
`tools/ocbm_install.sh`, which cost a lockout here too. This box has no `::respawn:` inittab entry,
so the trial supplies a detached restart loop.

### Failsafes, in order — OCBM itself beats any timer

1. **`CH_CONSOLE`.** `caps=0x3f` includes a root shell over the accessory link, so the box is never
   unreachable while `ocbmd` lives:
   `printf 'sync; /sbin/reboot -f\n' | ocbm-host console 1f3a ace2`. This is what recovered the
   2026-08-17 trial.
2. **A `setsid` timer script** — works, but **not reliably**. Fired exactly as designed in isolation
   (+45 s to the centisecond) yet did **not** fire during the trial. Cause unestablished. Not SIGHUP:
   detached processes demonstrably survive an adb session close here, unlike ssh/telnet on the CCPA.
3. **Unplug/replug.** Always works — see below.

## THE WATCHDOG RESETS THE BOX AT ~430 s FROM EVERY BOOT — AND THE FEEDER IS THE CAUSE

Measured five times: **427 s, 430 s, ~431 s with the full CarPlay stack, ~431 s on a BARE IDLE BOX
with nothing running, and once predicted in advance to the second.** The interval is anchored to
**boot**, not to any workload. An early "stack start + 300 s" correlation was pure coincidence — one
run started its stack at 80.7 s and still died at ~430 s, and two runs had no stack at all.

**It is not starvation.** `rc.preboot`'s feeder was measured at a perfect 20.008 s cadence from 3.4 s
uptime to 423.58 s — the last feed lands 4–7 s *before* the reset, and the `sleep 20` PID was seen
advancing the whole way. The feeder never misses.

**The feeder itself is the trigger.** The stock vendor rootfs never opens `/dev/watchdog` at all —
the 20 s feeder is an addition made by *this project's own* OCBM baseline `rc.preboot`. Best fit for
all five resets: the first open at 3.4 s **arms** the sunxi hardware watchdog, the subsequent pings
do not actually restart the silicon counter, and the period really programmed is ~427 s rather than
the 300 s the driver prints. 3.4 + ~427 ≈ 430, every boot. (Inference — the one-command proof is
below.)

**Fix:** stop feeding and disarm. `nowayout=0`, so the magic close is permitted. Kill the feeder
subshell *first* — otherwise its next `echo 1 > /dev/watchdog` re-arms the timer — then `echo V >
/dev/watchdog`. `c2air/tools/c2air_stack.sh` does this as its first action. The permanent fix is to
drop the feeder line from `rc.preboot` in the next squashfs build.

**Proof still outstanding (one command, needs hardware):** on a fresh boot, `echo V > /dev/watchdog`
and nothing else. Surviving past 431 s confirms the mechanism. A UART capture across the 431 s mark
would separate a hardware reset (silence → BROM banner) from a kernel-initiated reboot.

> [!WARNING]
> **An earlier version of this section claimed the exact opposite** — that the kernel's `[watchdogd]`
> keeps petting the device so killing the feeder could not reset the board, and to "leave the feeder
> alone". That reading of `watchdog: watchdog0: watchdog did not stop!` was inverted: the message
> means the watchdog **stayed armed** after userspace closed the node. Acting on the old advice is
> what left the ~430 s reset in place through five debugging sessions, and several conclusions drawn
> during those sessions were corrupted by resets landing mid-experiment.

### Why experiments here are cheap

**The rootfs is a read-only squashfs and the gadget is rebuilt at every boot**, so all state is
volatile: staging goes to `/tmp` (tmpfs, 28 MB), configfs is tmpfs-backed. Any reboot is a total
revert to a known-good ADB box in ~4 s.

Consequently there is **no `finalize` verb**. Persistence is not a file copy: it needs a rebuilt
squashfs flashed to **mtd2** via FEL (hidden button at power-on) or an SPI programmer. You cannot
`flashcp` the squashfs you are executing from, since its pages are read on demand.

**Never stage on `/mnt/UDISK`** — 316 KB free of 512 KB, and it holds the per-unit `carplay.key`.

## Reference

Full baseline documentation — partition layout, GPT rewrite traps, boot timing, the vendor toolchain,
and the OCBM dependency matrix — is in the resources repo at
`flash_dumps/c2air_v821_2026-08-17_ocbm_baseline/README.md`.
