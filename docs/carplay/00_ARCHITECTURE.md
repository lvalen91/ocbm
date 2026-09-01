# Architecture, vision and transport

> **STATUS:** CURRENT · single owner for this topic. Consolidated 2026-08-31 from pre-consolidation docs 00, 01, 03; the originals are in git history and in the 2026-08-31 backup. Correct this file in place — do not add a sibling.

**Contents:** what this project is → the box/host split → the USB/NCM transport layer.

## Vision — what this is and why

<!-- absorbed: ../carplay/00_ARCHITECTURE.md -->

### The problem

The Carlinkit CPC200-CCPA is a capable piece of hardware running **stock, feature-limited
firmware**. Its projection stack (riddleBox) speaks an **old CarPlay SDK** (Apple AccessorySDK
R14G17, ~2017), advertises a 2017-era capability set to the iPhone, and **strips** most of the
rich iAP2/AirPlay data before it reaches the host over a vendor-specific `0x55AA55AA` USB-bulk
wrapper. Modern CarPlay features (HEVC video, the wireless/buffered audio formats, alt/cluster
screens, enhanced Siri, vehicle state, …) are either never negotiated or thrown away.

The community's two blockers have always been (1) the **closed-source box software** and (2) the
**restricted old SDK**. Nobody has shipped an open, dongle-free, non-Apple-hardware CarPlay
receiver — the one public "it works" example requires a modified (jailbroken) Apple device — which this project explicitly avoids, and every other
approach needs a **custom PCB or a soldered MFi chip**. The community did converge on one hard
truth: you need the **genuine Apple MFi coprocessor** — real silicon, not software.

### What this is

`ccpa_custom` is an **open-source, Rust-first userspace** that replaces riddleBox entirely while
reusing what can't be replaced:

- the adapter's **radios** (WLAN/BT) and its onboard **genuine MFi 2.0C coprocessor**
  (`/dev/i2c-1` @ 0x11), used as an authorized accessory the way Apple's program intends;
- the **factory vendor boot stack** — HAB-signed U-Boot 2015.04 + per-chip OTPMK-encrypted
  kernel 3.14.52 — treated as fixed firmware and left untouched.

Everything above the kernel becomes open: a new bulk transport (**OCBM**), the CarPlay receiver,
the MFi authentication bridge, and the radio/link glue. The result is **open, current-iOS,
full-featured CarPlay on the real hardware** — no phone modification, no custom PCB, no soldered chip.

It's a **firmware + host-app pair**: the adapter (open userspace) and a host app that **claims the
adapter as a USB device** — exactly the isolated-USB-claim model of the original `carlink_macOS`,
never presenting the host OS a network interface. The design principle that makes it *last* is
**future-proofing by split**: the adapter owns only what is *hardware-bound or permanently-stable
mechanics* (the iAP2 link + MFi 2.0C auth + AirPlay pairing/key-derivation), and the **host app owns
everything configurable** — any parameter capable of multiple values, even ones stable across SDK
versions, authored app-side and pushed to the adapter at init — along with the parts Apple changes
between SDK versions (the SETUP negotiation, codecs/HEVC, decode, UI). So a new CarPlay feature is a
host-app change with little or no change to the adapter (docs/carplay/04_CAPABILITIES_AND_CONFIG.md). See
[`../carplay/00_ARCHITECTURE.md`](../carplay/00_ARCHITECTURE.md).

### What this is NOT

- **Not** a bootloader/kernel replacement. The boot ROM performs signature verification (HAB) and
  the kernel is OTPMK-encrypted; this project never touches either and works entirely in
  userspace above them.
- **Not** a from-scratch MFi implementation. Software-only MFi does not authenticate to iOS;
  this drives the on-board genuine coprocessor.
- **Not** a modification of anything on the iPhone.

### Why it meets the community goal

It threads the needle the field concluded was the only viable one — **use the real MFi silicon** —
but wraps it in fully open userspace with a modern, uncrippled stack, at the **lowest invasiveness
of any known approach**: obtain a root shell on the developer's own unit (the stock firmware's telnet/SSH login has no password set and involves no keys or certificates, over the local USB-NCM link; or use an SPI programmer), then install the open userspace. See [`../ops/01_RECOVERY.md`](../ops/01_RECOVERY.md) for the install/recovery model and
[`../carplay/00_ARCHITECTURE.md`](../carplay/00_ARCHITECTURE.md) for how the pieces fit.

---

## Architecture — the box/host split

<!-- absorbed: ../carplay/00_ARCHITECTURE.md -->

### The layering: open userspace on a fixed vendor floor

```
┌───────────────────────────────────────────────────────────────┐
│  OPEN USERSPACE (this project)                                 │
│   • OCBM bulk transport daemon  (owns /dev/usb_accessory)      │
│   • CarPlay receiver  (receiver_core, Rust)                    │
│   • MFi authentication bridge  (/dev/i2c-1 @ 0x11)                            │
│   • radio/link glue  (BT/iAP2, Wi-Fi SoftAP, handover, bridge) │
├───────────────────────────────────────────────────────────────┤
│  FIXED VENDOR FLOOR (treated as firmware — out of scope here)  │
│   • kernel 3.14.52  (per-chip OTPMK-encrypted, not modified)   │
│   • U-Boot 2015.04  (HAB-signed, not modified)                 │
└───────────────────────────────────────────────────────────────┘
```

Working above the fixed vendor floor has one practical implication: no kernel-level USB-gadget
flexibility (no configfs/gadgetfs/usable functionfs), so development is confined to the kernel's
**existing gadget function set** — which the `f_accessory` (bulk) and `f_ncm` functions already
cover. Nothing that defines the CCPA's behavior lives below userspace. See
[`../carplay/00_ARCHITECTURE.md`](../carplay/00_ARCHITECTURE.md).

### The committed model: split by configurability (docs/carplay/04_CAPABILITIES_AND_CONFIG.md), not by transport

> **⚠️ SUPERSEDES the earlier "Model A passthrough vs Model B decode-on-adapter" framing.** The
> canonical statement is repo `README.md` §Architecture; this is the architecture-doc detail. Full
> reasoning: [../ops/06_CORRECTIONS_LEDGER.md](../ops/06_CORRECTIONS_LEDGER.md) `R-01-1`.

CarPlay divides into a **hardware-bound / permanently-stable-mechanics** layer and a **configurable /
evolving** layer. Per docs/carplay/04_CAPABILITIES_AND_CONFIG.md the axis is configurable-vs-hardware-bound, not merely
stable-vs-evolving: a value that is stable in the protocol but *capable of multiple values* is still
app-side, and box placement is **earned, never designed-in** — app-driven first, moving to the box
only after a measured failure plus owner approval (the docs/carplay/04_CAPABILITIES_AND_CONFIG.md placement test):

#### The adapter owns the STABLE crypto foundation (autonomously)

- iAP2 accessory handshake (phone-facing USB/BT), **MFi 2.0C auth on the local chip** (`iap2d`).
- AirPlay **pair-setup / pair-verify** on the adapter's real `ncm0` interface (ordinary kernel
  sockets), using the MFi chip, deriving the **ChaCha20 session key(s)**.
- Hands the host app the **ephemeral session key** (a per-connection token) and **forwards the
  encrypted A/V untouched** over OCBM. The adapter never decodes and — in the committed design —
  need not decrypt the A/V lanes.
  **CORRECTED 2026-08-10 — the old sentence "It never parses CarPlay above pairing" was FALSE.** The
  box parses and ANSWERS RTSP SETUP phase 1/2 including the `enabledFeatures` echo
  (`receiver::session`), parses the encrypted `/command` event channel (`events.rs`), parses iAP2
  metadata TLVs and relays JSON to the host (`crates/vendor/metadata/*` → the :9004 seam), and
  decrypts the type-130 DataStream (`session.rs`, `datastream.rs`). Those are EARNED box placements
  under docs/carplay/04_CAPABILITIES_AND_CONFIG.md — hardware-bound, or simply not yet migrated to the app — not a claim of
  transparency. Describing it as transparency understated how much protocol actually lives on the box.

Apple keeps MFi 2.0C + the pairing lifecycle backward-compatible permanently (Apple is on MFi 4;
only genuinely MFi-3/4-gated *features* are unsupported, which we don't claim). So this layer never
changes — putting it on the adapter costs nothing in future-proofing.

#### The host app owns the EVOLVING session (where SDK changes live)

- Claims the accessory over USB (no OS interface), decrypts the A/V with the adapter-provided key.
- **Drives the post-pairing SETUP negotiation** (codecs/HEVC, resolution, screens, audio formats,
  features — see README §7), decodes HEVC (VideoToolbox), renders, forwards touch back.
- Holds the YAML config (single source of truth), pushing the adapter-relevant subset down over OCBM.

#### Crypto-coupling, resolved

Whoever completes `pair-verify` holds the ChaCha20 keys. The adapter does pair-verify (it holds the
MFi chip) and derives the keys, then **hands the app the ephemeral session key** so the app can
decrypt the untouched-forwarded A/V. The **MFi private key never leaves the chip**; only the
derived, throwaway session key crosses the link. (This is why the app can decrypt without the
adapter decrypting or stripping — "no stripping" is structural: our adapter code is open and
forwards everything.)

### Component map

| Component | Box (armv7, stable) | Host app (evolving) |
|---|---|---|
| OCBM transport daemon (`ocbmd`, owns `/dev/usb_accessory`) | ✔ | ✔ client |
| iAP2 handshake + MFi auth (`iap2d`, `/dev/i2c-1 @0x11`) | ✔ link mechanics + MFi | pushes identification *content* as config (docs/carplay/04_CAPABILITIES_AND_CONFIG.md); gets events |
| BT/iAP2 link + Wi-Fi SoftAP + handover | ✔ radios + link mechanics | pushes wireless credentials/config (docs/carplay/04_CAPABILITIES_AND_CONFIG.md) |
| AirPlay pair-setup/verify + ChaCha20 key derivation (`receiver_core` pairing path, on `ncm0`) | ✔ | — |
| Encrypted A/V forward + session-key handoff | ✔ | receives |
| SETUP negotiation (RTSP/codecs/resolution/features) | plumbing/relay only | ✔ drives (target) |
| Decrypt + decode (HEVC) + render + UI + input | — | ✔ |
| YAML config (source of truth) | receives subset | ✔ owns |

The radio-bound + MFi + pairing *mechanics* always stay on the adapter (hardware-bound). Their
configurable content — wireless credentials, identification/declaration content, every multi-valued
parameter — is app-authored and pushed at init (docs/carplay/04_CAPABILITIES_AND_CONFIG.md). The evolving SETUP + decode + UI live in
the app.

> **Status (2026-07-10):** the adapter half is built and hardware-validated end-to-end — iAP2/MFi →
> pairing/key-derivation → **encrypted A/V forward + session-key handoff** (0-failure host-side decrypt),
> under a host-app-driven lifecycle (see [`../carplay/02_SESSION_LIFECYCLE.md`](../carplay/02_SESSION_LIFECYCLE.md)) and a
> hardening pass that confirmed the crypto/protocol byte-for-byte against Apple's `CarPlaySDK`. The
> **host app** column (decrypt + decode/render + input uplink) is now IMPLEMENTED — the macOS app
> `host/CarPlayHost/carlink_macOS` (per-lane ChaCha20-Poly1305 decrypt, dual-lane decode/render, input
> uplink). The Rust `ocbm-host avdec` receiver is now a validation/debug tool, not the stand-in. See
> `../ops/04_OPEN_ITEMS.md`.

### Vendored assets (CORRECTED 2026-08-10 — the previous text pointed at an archived directory and at
C daemons that no longer exist)

The former sibling `ncm_carplayd/receiver_core` is now **vendored in-repo** as
`crates/vendor/{receiver, pairing, rtsp, mfi, mfi-i2c-local, iap2-core, metadata, wireless,
eld-codec, rx-connect}`; the original tree is archived at `../old/ncm_carplayd`. "Vendored" covers two
different things there: eight are path-dependency-only (`Cargo.toml` `exclude`), while `wireless` and
`rx-connect` are full workspace members producing the shipped `carplay-wireless` and `rx-connect`
binaries. The first-party crates outside that set are `crates/ocbm-proto` (the OCBM wire protocol,
shared by every daemon) and `crates/mfi-wire`.

The box daemons in `ccpa/{ocbmd, airplayd, iap2d, mfid}` are **all Rust** — the earlier "keep the
low-level glue in C" plan (`../ops/00_BUILD_AND_DEPLOY.md` §Recommendation) was NOT taken: no C MFi bridge,
no C iAP2/radio glue and no C L3/NCM bridge was ever written, and `find ccpa -name '*.c'` still
returns nothing. The shipped box set is five Rust binaries (`ocbm_install.sh --full`: `ocbmd`,
`iap2d`, `airplayd`, `rx-connect`, `carplay-wireless`; `mfid` is a bring-up instrument, staged to
`/tmp` and never installed) plus one small C helper.

**CORRECTED 2026-08-16 — the previous text claimed `eld_shim.c` was "the only C remaining in-tree".
That was false.** Four C-language files are tracked, ~318 lines total, all deliberately small and
peripheral to the daemons (note `git ls-files '*.c'` alone misses the fourth):

- `crates/vendor/eld-codec/csrc/eld_shim.c` (75) — the libfdk-aac AAC-ELD FFI shim, deliberately
  isolated so `receiver` can stay `#![forbid(unsafe_code)]` (`receiver/src/lib.rs:10`). Compiled by
  `eld-codec/build.rs` into `airplayd`.
- `accessory_init/iap_role_switch.c` (45) — a raw-usbfs `USBDEVFS_CONTROL` issuing Apple's `0x51`
  host-role switch and exiting; libusb-free so it runs on the stripped appliance. **The one C binary
  we ship**: built by `build.sh:46` (`zig cc`, static armv7), installed to `/usr/bin` by
  `install_fhs.sh:20`, and invoked on every wired bring-up by `projection_up.sh:36` (which
  `session_supervisor.sh:199` runs on each SUBSCRIBE). ⚠ `ocbm_install.sh --full` ships
  `projection_up.sh` but **not** this binary — a box provisioned that way fails wired bring-up.
- `host/CarPlayHost/carlink_macOS/USB/USBBridge.h` (115) — the macOS app's Swift bridging header:
  inline C wrappers for IOKit CFUUID macros that Swift's ClangImporter cannot bridge. Compiled by
  Xcode into the shipped app.
- `host/accbench.c` (83) — host-side libusb throughput benchmark, source of the transport numbers in
  `../carplay/00_ARCHITECTURE.md`. Hand-built per `host/README.md:18`; `build.sh` never touches it.

Not in-tree but linked in: `airplayd`'s default `mic-uplink-eld` feature statically links a
cross-built **fdk-aac 2.0.3** (~840 C/C++ files) from `$FDK_AAC_PREFIX`, gitignored under
`scratchpad/fdk/`. The gitignored `reference/` and `scratchpad/` trees are third-party and not
counted here.

---

## Design decisions and rejected alternatives

Recorded so future sessions understand *why* and don't revert:

- **REJECTED — host presents an OS network interface (`utun`/`feth`+BPF/`ncm`).** Violates the
  isolated-USB-claim model. The host must do everything inside the app over the claimed device.
- **REJECTED — app-side userspace TCP/IP stack (`smoltcp`) terminating raw forwarded frames.** This
  was on the table when we thought the app had to drive pairing. Once the adapter owns pairing (it's
  stable), the adapter terminates on its real `ncm0` and there is no need for a userspace stack on
  either side. (The `CH_ETH` raw-frame bridge that this idea produced still exists and is proven — it
  is now a **diagnostic**, not the A/V path.)
- **REJECTED — the adapter strips/limits the protocol (what riddleBox did).** Our adapter code is
  open and forwards everything; "stripping" was a closed-source Carlinkit behavior, not a constraint.
- **REJECTED — the adapter decodes A/V.** The i.MX6UL has no VPU; decode is always app-side
  (VideoToolbox). The adapter forwards encoded NALs.
- **KEPT — the adapter owns MFi + pairing + key derivation (stable), the app owns decode + evolving
  SETUP + UI + config.** This satisfies: MFi autonomous on the adapter, A/V untouched (app decrypts
  with the handed-over ephemeral key), and future-proofing (evolving layer app-side).

---

## Transport — USB gadget, NCM, the accessory node

<!-- absorbed: ../carplay/00_ARCHITECTURE.md -->

All facts below are **live-verified 2026-07-08** on an A15W unit (i.MX6UL, kernel 3.14.52 armv7l),
Mac acting as USB host, over the host-facing gadget port.

### The gadget mechanism

- Host-facing gadget = UDC **`ci_hdrc.1`**, a legacy monolithic `android_usb_accessory` gadget
  driven via sysfs `/sys/class/android_usb_accessory/android0` (`enable`, `functions`, `idVendor`,
  `idProduct`, `state`). Set a function: `echo 0 > enable; echo <funcs> > functions; echo 1 > enable`.
  **A composite list is *syntactically* accepted but `accessory,adb` does not enumerate** — see
  "Composite gadget: tested, does not work" below. `functions` is comma-separated (a space-separated
  list is silently accepted and leaves the list EMPTY), and is `EBUSY` while `enable=1`.
- **Fixed, kernel-compiled function set:** `f_accessory`, `f_adb`, `f_mtp`, `f_mass_storage`,
  `f_ncm`. Each exposes a misc (major 10) char device: **`f_accessory` → `/dev/usb_accessory`
  (10,56)**, `f_adb` → `/dev/android_adb` (10,57), `f_mtp` → `/dev/mtp_usb` (10,58).
- **No configfs/gadgetfs/usable functionfs**, and the kernel (fixed by the vendor boot stack) has
  no support for defining arbitrary interfaces/endpoints. A custom bulk transport must repurpose
  one of these existing function char-devices. `f_accessory` is the stock/proven one
  (`ARMadb-driver` opened `/dev/usb_accessory`).

### Accessory-mode descriptors

| Field | Value |
|---|---|
| VID / PID | **`0x1314` / `0x2d00`** (hex; macOS "Auto Box" / "Magic Communication Tec.") |
| bDeviceClass | `0x00` |
| Interface 0 | class `0xFF` / sub `0xF0`, 2 endpoints |
| Bulk IN | **`0x81`**, wMaxPacketSize **512** (was documented `0x83` — see note) |
| Bulk OUT | **`0x01`**, wMaxPacketSize **512** (was documented `0x02` — see note) |
| Speed | USB 2.0 **high-speed** |
| iSerial | **`0123456789FEDCBA`** — vendor placeholder baked into the gadget module |

> **Endpoint-address correction (2026-08-17, live on GM `gminfo37`).** Two independent Android hosts —
> the `:usbhandler` proof app and the `zeno.carlink.ocbm` app's own USB transport — both enumerated this
> CCPA-OCBM as **bulk IN `0x81`, bulk OUT `0x01`** (interface 0, class `0xFF`, mps 512), logged in
> `~/Downloads/log.txt`. The earlier `0x83`/`0x02` in this table is not what the shipped gadget presents
> on this unit. Functionally moot — every host here discovers the bulk pair by walking interface 0's
> endpoints rather than hard-coding, and both claimed and ran a full OCBM session — but do not hard-code
> `0x83`/`0x02` anywhere; enumerate. If an older CCPA firmware genuinely presented `0x83`/`0x02`, note
> the per-build variance rather than assuming one value.

Host side: claim IF 0 (macOS/Linux libusb, Android `UsbManager`), transfer on the enumerated bulk pair
(**`0x81` IN / `0x01` OUT** as measured; walk the endpoints, do not assume).
**No AOA control handshake (51/52/53)** is needed — claim and go. It is a **raw byte pipe**; all
framing is application-defined (this is where OCBM lives).

#### Composite gadget: tested, does NOT work — ADB and OCBM cannot coexist on this kernel

**Tested live 2026-08-17 on the CCPA over the OCBM link.** The question was whether the legacy gadget
could present `accessory` + `adb` together, so a host could speak OCBM and drive `adb shell` on the
same cable (management without leaving OCBM). It cannot.

What the driver does with a composite list:

| `functions` write | `rc` | readback | result |
|---|---|---|---|
| `accessory,adb` | 0 | `accessory,adb` | accepted, but see below |
| `adb` | 0 | `adb` | accepted |
| `adb,accessory` | 0 | `adb,accessory` | accepted, order preserved |
| `accessory adb` (space) | **0** | **`[]`** | **silently EMPTIES the list** — footgun |
| any write while `enable=1` | 1 | unchanged | `EBUSY` — must `echo 0 > enable` first |

But enabling `accessory,adb` **destroys the OCBM transport and never enumerates**:

```
COMPOSITE: enable=1 state=CONNECTED functions=accessory,adb class=0
nodes: acc=/dev/usb_accessory: No such file or directory   adb=/dev/android_adb
[ ...] android_usb_accessory gadget: acc_function_disable
[ ...] android_usb_accessory gadget: _acc_poll POLLHUP!
```

- Adding `adb` fires `acc_function_disable`: **`/dev/usb_accessory` is torn down** and ocbmd gets
  `_acc_poll POLLHUP!`. OCBM's fd is gone the instant adb joins the list.
- The device stops at **`CONNECTED`**, never `CONFIGURED` — the host sees an attach but never completes
  `SET_CONFIGURATION`, so it does not appear on the bus (confirmed: host `ioreg` showed no `0x2d00`
  during the composite window).
- `bDeviceClass=239` (IAD) does **not** rescue it — same `CONNECTED` dead end. So this is not the
  macOS-seizes-IAD problem; the accessory+adb combination itself does not come up on this kernel's
  monolithic gadget.

**Consequence:** on the CCPA, `adb` is only ever an *alternative mode* (like `ncm`), never a companion
to OCBM. And it buys little as an alternative: OCBM already provides the management channel (shell via
`ocbm-host console`, file transfer via `push`/`pull`), and `adb`/`ncm`/`accessory` are all functions of
the **same** `g_android_accessory` driver — so `adb` would fail for the same reasons a broken gadget
fails, giving no independence. NCM at least brings up a separate IP stack (ssh/scp/port-forward +
`tools/boxsh.py`) and is installed and proven. The independent recovery channels remain UART and the
SPI programmer. (Contrast the C2Air, where ADB *is* the primary channel and coexists with ACM — that
platform has real **configfs**, not this legacy monolithic gadget.)

#### Why the PID differs from stock, and what hosts must do about it

Stock/NCM is `0x1314:0x1520` (`0x1521` on some SKUs); OCBM is `0x1314:0x2d00`. **The PID change is
load-bearing and is not just installer bookkeeping.** Two reasons, in order of importance:

1. **It marks a different application protocol on the same wire.** The endpoints and interface class
   are identical between stock and OCBM, so a PID is the only thing in the descriptors that tells a
   host "this pipe no longer speaks the Carlinkit protocol." Anything that claims `0x1520` expecting
   the old framing must NOT match a converted box. This is what keeps
   `carlink_native` (the native Kotlin implementation of the **original** Carlinkit protocol, which
   contains no OCBM code) from claiming a converted adapter — its `usb_device_filter.xml` lists
   `0x1520`/`0x1521` only, and that omission is **correct by design**. Do not "fix" it.
2. Secondary: it gives `ocbm_install.sh` a mode test that is not satisfied by the very device being
   replaced (see the comment at its enumeration wait — a vendor-only match returns immediately
   because NCM is the same VID).

**Consequence for host apps that DO speak OCBM:** they must list `0x1314:0x2d00` in their
`res/xml/usb_device_filter.xml` *and* in any runtime VID/PID allowlist, or the Android
`USB_DEVICE_ATTACHED` intent never matches, the app is never launched by attach, and the AOSP
implicit per-device permission grant never happens — leaving only the explicit
`UsbManager.requestPermission()` dialog on every connect. `host/CarlinkAndroid` lists it
(`11520` decimal); a new OCBM host app must too.

> **Caveat on the number itself.** `0x2d00` is *Google's AOA accessory PID* (AOA uses VID `0x18d1`
> with PID `0x2d00` = accessory, `0x2d01` = accessory+ADB; the stock firmware's own `start_aoa.sh`
> sets exactly `18d1:2d00`). Under VID `0x1314` it should not trigger Android's AOA handling, which
> keys on the Google VID — but it is an avoidable ambiguity and remains an unexamined confounder in
> the GM AAOS permission investigation. A PID in Carlinkit's own range (e.g. `0x1522`) would carry
> the same "different protocol" signal without borrowing Google's number. Changing it now costs a
> reflash plus updates in `ocbm_boot.sh`, `ocbm_install.sh`, and every host app's filter.

#### iSerial: present, stable, and NOT a lever for host-side permission persistence

Measured on a live converted unit — box side
`/sys/class/android_usb_accessory/android0/iSerial` and host side
`ioreg -p IOUSB -l` → `"USB Serial Number" = "0123456789FEDCBA"`. So the adapter **does** present a
serial to the host, it is **stable across power cycles** (nothing in our scripts ever writes
`iSerial`), and it is **identical on every CCPA** because it is a vendor placeholder rather than a
per-unit value.

This refutes two things that had been assumed elsewhere: that the adapter presents no serial (the
`USBSerial` config key defaults to `""` and `iSerialNumber` is absent from the observed-descriptor
table in the resources repo, from which "no serial" was inferred — the gadget module supplies the
placeholder regardless), and that giving it a serial would make an Android host remember a USB
permission grant. It already has one, and the grant still is not remembered — so **iSerial is not
the cause and changing it is not the fix.** The only thing a per-unit serial would buy is
distinguishing two adapters on one host.

### Throughput (measured, libusb host ↔ box `dd`)

| Direction | Endpoint | Rate |
|---|---|---|
| Adapter → host (device write) | `0x83` IN | **339 Mbps** (254 MB / 6.0 s) |
| Host → adapter (device read) | `0x02` OUT | **90 Mbps** (65.6 MB / 5.8 s) |

Both far exceed CarPlay's need (video 8–30 Mbps). The write asymmetry is the i.MX6UL ChipIdea
receive/`acc_read` path being less optimized than transmit. Coordination for the OUT path: post the
device-side read **before** the host writes; use large block sizes (`bs=64K` — `bs=512` collapses
write to ~10 Mbps).

### Operational hazard

Disabling the accessory gadget (`echo 0 > enable`) while **OUT transfers are pending/undrained
HANGS the gadget-teardown** in an uninterruptible kernel wait → wedges the box (console D-state,
USB drops, no software recovery) → **power-cycle only**. Safe practice: post the read before
writing; **revert via `reboot -f`, never `echo 0`**; wrap on-box experiments in a cancelable
reboot-watchdog `(sleep N; [ -e /tmp/keep ] || reboot -f) &` so any hang self-recovers.

### The two physical ports

- **`ci_hdrc.1`** = host-facing **MALE** USB-A cable (Type-C female on some models). CCPA is
  **always a gadget** here (car/head-unit/Mac is host). VID `0x1314`. This carries OCBM.
- **`ci_hdrc.0`** = the **FEMALE** USB-A port (phone or USB storage). **OTG dual-role**: EHCI host
  by default (USB storage / hosting) and peripheral mode presenting `iap2,ncm` (VID `0x08e4`) for a
  wired phone. Relevant to wired-CarPlay ingest, not to the host-facing OCBM link.

For the full hardware/gadget writeup see the community repo: `CPC200-CCPA_resources/documentation/
01_Firmware_Architecture/hardware_platform.md` § Host-Facing Gadget.
