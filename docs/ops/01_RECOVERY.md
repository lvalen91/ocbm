# Recovery

> **STATUS:** CURRENT · single owner for this topic. Consolidated 2026-08-31 from pre-consolidation docs 04; the originals are in git history and in the 2026-08-31 backup. Correct this file in place — do not add a sibling.

Getting a bricked or wedged box back, without touching the signed boot stack.

## Recovery procedures

<!-- absorbed: ../ops/01_RECOVERY.md -->

### Recovery tiers (most robust first)

| Tier | Channel | Survives | Needs |
|---|---|---|---|
| 1 | **SPI programmer** (hardware) | any state (brick-proof) | physical flash access |
| 2 | **UART console** (`ttymxc0`, TX1/RX1 pads) | dead userspace, wedged gadget, hung projection | kernel alive |
| 3 | **OCBM `CONSOLE` mode** (this project) | any failure *downstream of the mode branch* — MFi/WiFi/iAP2/receiver | OCBM daemon + accessory gadget alive |
| 4 | NCM telnet (no login) / NCM SSH (no password) / the same two over the Wi-Fi AP | least | gadget + netdev + DHCP + daemon, or hostapd + radio |

Each lower tier rescues cases the tiers above it cannot. OCBM `CONSOLE` is *much* more robust than
NCM/WiFi (it touches the network stack zero times and rides the primary transport) but does **not**
replace UART (kernel-level) or the programmer (hardware) — it shares fate with the gadget + daemon.

### OCBM `CONSOLE` mode

A host-selected state, not a background channel (see [`../carplay/01_OCBM_PROTOCOL.md`](../carplay/01_OCBM_PROTOCOL.md)
§ Modes). At connect the host sends `MODE_SELECT { CONSOLE }`; the box branches **before any
projection machinery** and wires the accessory pipe straight to a root PTY (`/bin/sh`, root — the
daemon already runs as root). The earlier the branch, the fewer subsystems it depends on, the more
failures it survives.

Solves the two target scenarios directly:
- **"NCM won't activate"** — no `ncm0`, no DHCP, no netdev, no `dropbear`. Rides the accessory
  gadget the box already presents.
- **"Wi-Fi AP won't come online"** — no `hostapd`, no radio.

Rescue tool: **`ocbm-host console`** — there is no separate `ocbm-rescue` binary; that role shipped as
a subcommand of the one host client (`mode_console`, `host/ocbm-host/src/main.rs:356`), which claims
the accessory interface, sends `MODE_SELECT { CONSOLE }`, and bridges the PTY to your terminal (raw
mode). It works even if the main host app is broken, and `tools/ocbm_install.sh finalize` relies on it
for the first-boot dead-man confirm. *(Its log lines still carry the historical `[ocbm-rescue]` tag,
`main.rs:372`.)* Nice-to-haves for cheap: a `resize` TLV (window size/`TERM`), multiple sessions via a
session id in the CONSOLE payload. (Neither exists today; there is no `streamId` field anywhere in
OCBM — see docs/carplay/01_OCBM_PROTOCOL.md §Self-describing streams.)

#### Access model (matches the stock firmware's existing open-access posture)

The console mode adds no additional authentication because the platform already grants unauthenticated local access (see below). The stock vendor firmware already provides open local access (an SSH/telnet login with no password set, a root serial console, a read-write rootfs, and field-reflashability), so local physical access already implies full control by design. Providing a console over the accessory link therefore does not change the access model — it uses the same physical-USB trust boundary. Real protection
would require deep encryption that is explicitly out of scope (and the HAB/OTPMK floor sits below
all of it regardless). `MODE_SELECT` also prevents accidental entry: default is `PROJECTION`; only
our host requests `CONSOLE`.

### Install / deploy models

Getting the open userspace onto a unit needs **no credential** — the access model above *is* the
install model. The unit ships a single account (root) with no password set, and `rcS` re-arms both
shell servers on every boot, so this is the standing configuration rather than a bootstrap window
that closes behind itself.

1. **Live overlay** — take a root shell over USB-NCM (unauthenticated telnet is the default transport
   for the base install; SSH with the unset root password is preferred by `tools/ocbm_install.sh` for
   real exit status and single-`cat` transfers — **no keys or certificates are involved on either
   side**), then copy the daemons onto the read-write jffs2 rootfs. No programmer required. Best for
   iteration. The optional Wi-Fi-AP backstop is the one place a credential exists, and only at the
   link layer: a device-derived WPA2 PSK. The shell behind it is the same open login.
2. **Flashed `mtd2`** — bake the daemons into a custom jffs2 rootfs image, program it with the SPI
   programmer. Reproducible image + guaranteed brick recovery. Best for release.

The partition layout is fixed by the vendor kernel/U-Boot (`mtd0` 256 K uboot / `mtd1` 3328 K
kernel / `mtd2` 12800 K rootfs); you control everything inside `mtd2`. See
[`../ops/00_BUILD_AND_DEPLOY.md`](../ops/00_BUILD_AND_DEPLOY.md).

#### Persistent FHS install — the layout that ships

**Commission a unit with `tools/ncm_base_install.sh` then `tools/ocbm_install.sh`**, whose
`manifest()` is the authoritative file set. `tools/install_fhs.sh` is NOT the commissioning path any
more — it is the quick live-overlay refresh of an already-commissioned box. The FHS layout it lays
down is still what ships:

The rootfs is **jffs2 (rw)**, so the open userspace persists across reboots. `tools/install_fhs.sh`
lays it out FHS-style (not binaries-in-`/script`) and repoints the boot hook:

- **daemons → `/usr/sbin/`** — `ocbmd`, `iap2d`, `airplayd`, `rx-connect`;
- **tools → `/usr/bin/`** — `iap_role_switch`;
- **scripts → `/script/`** — `ocbm_boot.sh` (boot hook, launches ocbmd + `session_supervisor.sh`),
  `session_supervisor.sh` (the lifecycle actor), `projection_up.sh` (IDLE→projection bring-up),
  `phone_reset.sh`, `peer_store.sh`, `carplay-status.sh`, and the PID-1 respawn wrappers
  `run_ocbmd.sh` / `run_supervisor.sh` (`install_fhs.sh`'s script loop is the authoritative list).

The installer is ETXTBSY-safe and never drops exec bits. Once installed, a **cold boot comes up with a
live OCBM link and zero bootstrapping** — reboot-proven (Mac gets `HELLO_ACK` with no UART base64
deploy). Note the box `/tmp` is tmpfs (lost on reboot); only the FHS-installed files survive.

#### Boot chain (turnkey — how the box reaches a live OCBM link)

`busybox init` → `/etc/inittab` → **`::sysinit /script/early_console.sh`** (UART root console, FIRST —
the Tier-2 recovery path) + `/etc/init.d/rcS` (mounts, dropbear/telnetd, mdev, network) →
**`/script/start_main_service.sh &`** → (no `ncm_only`/`ncm_wifi` flag ⇒ OCBM appliance default)
**`/script/ocbm_boot.sh`** → arms the host-facing `accessory` gadget (bDeviceClass=0, PID `0x2d00`) →
launches **`/usr/sbin/ocbmd`** + `session_supervisor.sh` (idle-waiting on `/tmp/host_present`).
`ocbm_boot.sh` backgrounds and `exit 0`s so it can never block boot. PID 1 then keeps both alive
independently of that launch: `/etc/inittab` carries `::respawn:/script/run_ocbmd.sh` and
`::respawn:/script/run_supervisor.sh` (task #28) — wrappers that relaunch only a *dead* daemon, so a
crash or OOM costs a restart, not a reboot. `tools/ocbm_install.sh finalize` is what appends them, and
it refuses to touch `inittab` unless `/script/run_ocbmd.sh` is present and executable.
On-demand pieces (iap2d cold-start
via `projection_up.sh`, airplayd/rx-connect via the supervisor) are **not** started at boot — the box
boots to IDLE and only projects when a host app SUBSCRIBEs (see [`../carplay/02_SESSION_LIFECYCLE.md`](../carplay/02_SESSION_LIFECYCLE.md)
and [`../carplay/07_PHONE_SIDE.md`](../carplay/07_PHONE_SIDE.md)).

### UART console reference

Pads **TX1/RX1** (+ GND) = i.MX6UL UART1 = `ttymxc0`, 3.3 V logic, always-on root serial console (physical-pad access only)
shell. **115200 8N1** — `/script/early_console.sh` `stty`s `ttymxc0` to 115200 unconditionally inside
its respawn loop before the login shell, and every host-side tool assumes it (`host/uart_cmd.sh:8`,
`tools/uart_push.sh:19`). *(**CORRECTED 2026-08-16** — this read "Default **9600 8N1** (a persistent
115200 wrapper via inittab is available)". Whatever state that described predates this repo's
history, which has shipped `early_console.sh` since the 2026-07-25 baseline; there is no 9600 state
on a converted box.)* U-Boot 2015.04
is silenced on the pins (`console=ttyLogFile0`), so nothing prints during boot — only the post-boot
Linux shell. `/script/early_console.sh` runs as the **first** `::sysinit` in `/etc/inittab` (before
`rcS`), so the UART shell is available even if `rcS` itself is broken (e.g. a script that lost its exec
bit). This is the Tier-2 floor for when the gadget/daemon itself is dead.
