# Pi Zero 2 W — bring-up platform for an alternative OCBM box

Third board in this repo after the CPC200-CCPA (`ccpa/`) and the C2Air (`c2air/`).
**Raspberry Pi Zero 2 W**: BCM2710A1 (RP3A0), 4x Cortex-A53 @1 GHz, **512 MB**, CYW43438 radio,
one data-capable micro-USB (dwc2 OTG) plus one power-only micro-USB.

> [!IMPORTANT]
> This subtree is **bring-up only**. Nothing here implements OCBM yet. The purpose is to get a
> minimal Linux up, get a control channel over the OTG port, and *measure* the six board facts the
> OCBM port depends on — before writing any port code. `c2air/README.md` is the precedent: that
> port cost almost no board-specific Rust because three hardcoded board facts happened to match.
> On this board **they do not match**, and the honest thing is to establish that with a report
> rather than an estimate.

## Where this board diverges from the CCPA (read before planning any port)

| Fact the daemons depend on | CCPA | C2Air | Pi Zero 2 W |
|---|---|---|---|
| MFi 2.0C coprocessor | `/dev/i2c-1 @0x11` | `/dev/i2c-1 @0x11` | **absent** |
| Host-facing bulk transport | `f_accessory` -> `/dev/usb_accessory` | `f_accessory` | **no `f_accessory` in mainline** — functionfs instead |
| USB gadget state | `/sys/class/android_usb*/android0/state` | same | **`/sys/class/udc/<udc>/state`** (configfs) |
| USB **device** controllers | 2 (phone + host) | 2 | **1** |
| Wi-Fi | 2.4 + 5 GHz | 2.4 + 5 GHz (AIC8800) | **2.4 GHz only** (CYW43438) |
| WLAN interface | `wlan0` | `wlan0` | `wlan0` — matches |
| Bluetooth | `ttymxc2` @115200 H5 | `ttyS1` @500000 H4 | PL011 serdev, in-kernel attach — **no `btattach` equivalent needed** |

Four of those are real work; two are free. Taking them in order of how much they constrain the design:

1. **No MFi coprocessor. No session without one.** Ranked by what they cost:
   - **Wire a genuine MFi 2.0C to I2C-1 (GPIO2/3) at `0x11`.** Costs no Rust: that bus and address
     are *already* the hardcoded target in `ccpa/iap2d` and `crates/vendor/wireless/src/mfi_local.rs`.
   - **`CARPLAY_MFI_ADDR` -> `ccpa/mfid` on an attached CCPA**, as the Pi 4 / AAOS port does
     (`pi/docs/00_PI_AAOS_PORT.md` §1). Proven, but it needs an IP path to that CCPA, and on a
     one-controller board the USB port is already the OCBM link and `wlan0` is already the phone's AP.
   - **Neither OCBM channel solves this as written, and it is worth being explicit about why**, since
     both look like they should. `CH_MFI` serves the *host* from the *box's* chip — `handle_mfi` in
     `ccpa/ocbmd/src/main.rs` reads `self.mfi`. `CH_IP` is a stream mux whose `OPEN` is handled by
     `ocbmd`, so relays are host-initiated. Both run the wrong way for a chipless box. Making either
     reversible is small work and would let the Pi borrow the Mac's attached CCPA over the link it
     already has — but it is work, not configuration.

   `pizero_verify.sh` probes `0x11`, so which of these applies is a measurement, not a plan.
2. **One USB device controller — this is what decides the board's role.** The "PWR IN" micro-USB
   has no data lines; the inner one marked "USB" is a full dwc2 OTG port, but there is only the one.
   The CCPA spends two controllers, and `docs/wireless/00_WIRELESS_CARPLAY.md` §3.1 says exactly what on:
   - the **host-facing gadget** carries OCBM to the macOS app — *"this leg never changes"*;
   - the **phone-facing OTG** is wired CarPlay only. It starts as a USB **host** to send the `0x51`
     vendor control transfer (`accessory_init/iap_role_switch.c`), the iPhone becomes host, and the
     box flips to a **gadget** presenting `iap2,ncm` (`tools/projection_up.sh:34`).

   So the phone leg needs a controller that is host-then-device and the host leg needs one that is
   always a device. On one controller you can have either, never both:

   | Role | Ports needed | Pi Zero 2 W |
   |---|---|---|
   | **Wireless-only accessory** | 1 (OCBM gadget; phone arrives over BT then Wi-Fi) | **fits** |
   | Wired CarPlay accessory | 2 | no |
   | Dual-transport arbitration (`docs/wireless/00_WIRELESS_CARPLAY.md` §3 design intent) | 2 | no |

   **Wireless-only is therefore the role this board can actually fill**, and it is not a degenerate
   one — it is the transport the whole `crates/vendor/wireless` + `airplayd` stack was built for.

   The *endpoint* budget on that one controller is not a second constraint, though it looks like one.
   `dwc2 3f980000.usb: EPs: 8, dedicated fifos, 4080 entries in SPRAM` reads like a pool of eight,
   but dwc2 keeps separate `eps_in[]`/`eps_out[]` arrays and the practical limit is TX FIFO space.
   **Measured 2026-08-22:** NCM (3) + ACM (3) + a bulk pair (2) — the same shape as NCM + ACM + an
   OCBM functionfs interface — bound together at high speed with `usb0` and `ttyGS0` both live.
3. **2.4 GHz only — a constraint, not the wall this document first called it.** `docs/wireless/00_WIRELESS_CARPLAY.md` §2.6 quotes
   WWDC 2017-717: 802.11ac / 5 GHz is **recommended**, not required. The vendor firmware ships
   `/etc/wifi_use_24G`, which forces channel 6 (`docs/wireless/01_BT_AND_RADIO.md`:254), and per `docs/carplay/04_CAPABILITIES_AND_CONFIG.md` the band is an
   app-pushed config value. What 2.4 GHz genuinely costs is in the same `docs/wireless/00_WIRELESS_CARPLAY.md` table: **BT must be
   off during an active session**, because the coexistence allowance for extra BT profiles only
   applies to a 5 GHz AP. That is survivable here — BT is a bootstrap channel that carries the
   `0x5702`/`0x5703` handoff, after which wireless metadata rides the AirPlay DataStream (stream type
   130, `docs/carplay/05_METADATA_AND_CONTROLS.md`) rather than BT. Verify before relying on it; `pizero_verify.sh` dumps the `iw phy`
   band list so the radio's actual capability settles this, not the datasheet.
4. **`f_accessory` does not exist in mainline Linux** — it is an Android gadget patch. The OCBM
   transport would have to be re-presented over **functionfs**: one interface, class `0xFF`, two
   bulk endpoints, mps 512. That is a faithful reproduction of what `docs/carplay/00_ARCHITECTURE.md` records
   the CCPA presenting, and the host side already walks interface 0's endpoints instead of
   hard-coding them, so the host needs nothing but the right VID/PID. `pizero_verify.sh` brings a
   real functionfs gadget up and reads back its descriptors, so the transport question is answered
   before any `ocbmd` backend is written.

What comes free: `wlan0` is the interface name the daemons already expect, and Bluetooth attaches
in-kernel via serdev, so there is no `btattach` to write.

## ADB, and why Debian's adbd cannot be used as shipped

Target set is **ADB + OCBM**. Until OCBM exists the config is **ADB + NCM + ACM**, 8 endpoints —
the shape already measured to bind — so nothing is dropped early.

`adbd (34.0.5-12)` is a real Debian Trixie package, so this costs an `apt install`, not an AOSP
build. But **its systemd unit cannot work on this board**, and the reason generalises:

    ExecStartPre=/usr/lib/android-sdk/platform-tools/adbd-usb-gadget setup

That helper builds its **own** gadget, `g1`, and then `activate` does `echo <udc> > g1/UDC`. On a
board with one controller that we already own, the write fails:

    adbd-usb-gadget[2909]: ...adbd-usb-gadget: 56: echo: echo: I/O error
    adbd.service: Control process exited, code=exited, status=1/FAILURE

adbd *itself* was fine — it had already opened `/dev/usb-ffs/adb/ep0`. Only the gadget half clashed.
The fix is a drop-in that clears all three of Debian's hooks and points them at `pizero-gadget`,
which puts `ffs.adb` in **our** config instead of building a rival gadget.

**functionfs forces the setup/activate split.** A gadget carrying an ffs function cannot bind to the
UDC until the daemon behind that function has written its descriptors to `ep0`. So `pizero-gadget`
has no single `start` in the service path:

| Step | Who | What |
|---|---|---|
| `setup` | `pizero-gadget.service` | create the gadget, mount functionfs at `/dev/usb-ffs/adb`, **no UDC write** |
| — | `adbd` | opens `ep0`, writes descriptors, waits |
| `activate` | `adbd.service` `ExecStartPost` | write the UDC — all three functions enumerate at once |

`activate()` falls back to binding **without** `ffs.adb` if the full bind fails, and a 60 s
`pizero-gadget-fallback.timer` binds anyway if adbd never starts. On a board whose other channels
are functions of the same gadget, a broken adbd must not cost the link.

Two properties worth knowing:

- **`adb shell` lands as uid 0.** The `adbd_auth` socket is absent on Debian (`socket unavailable,
  disabling user prompts`), so adbd runs unauthenticated and root. Convenient on a bench, and a
  reason not to expose this board's USB port to anything untrusted.
- **The endpoint budget is not the constraint** the four-way table below implies — but ADB + NCM +
  ACM + OCBM is still 10 endpoints against a proven 8, so OCBM's arrival is what retires ACM.

## Why NCM stayed, rather than ADB replacing it

ADB is the better *management* channel — it survives the UDC rebinds that OCBM bring-up causes on
every iteration, where NCM and ACM both drop — and it matches the rest of the bench, where the Pi 4
AAOS, Pi 5 AAOS and C2Air are all adb. But NCM earns its 3 endpoints separately:

- A real IP link is what an outbound `CARPLAY_MFI_ADDR` needs, and `scp`/port-forwards with it.
- The repo's NCM conventions (`tools/ncm_base_install.sh`, `tools/boxsh.py`) transfer directly.
- Unlike the CCPA's monolithic gadget — where `docs/carplay/00_ARCHITECTURE.md` proved `accessory,adb` tears
  the transport down and never reaches CONFIGURED — dwc2 + configfs composites cleanly. **Measured
  here: `acm.gs0 ffs.adb ncm.usb0` bound simultaneously**, which is the property the whole plan
  rests on.

**Addressing is `192.168.51.0/24`, not `.50.0/24`.** The CCPA sits at `192.168.50.2` and is the
likely MFi oracle for this board (item 1), so both must be attachable to the same host at once.

Three ways in — but read the third column before trusting the word "independent":

| Channel | Address | Survives | State 2026-08-22 |
|---|---|---|---|
| USB NCM + SSH | `192.168.51.2`, or `fe80::51:ff:fe00:2%<iface>` | a broken network config? **no** | live |
| USB ACM console | `/dev/cu.usbmodem*` @115200 | a broken *network* config, not a broken gadget | live |
| GPIO UART console | GPIO14/15 @115200 | a broken gadget, a broken boot, the firmware stage | **not wired** |

> [!WARNING]
> **NCM and ACM are not independent of each other.** They are two functions of one configfs gadget on
> the one controller: a failed `UDC` bind or a bad functionfs descriptor set takes both down at the
> same instant. Until the GPIO UART is physically wired, the board has *no* channel that survives a
> broken gadget, and every gadget experiment needs an unconditional dead-man revert — the pattern
> `tools/ocbm_install.sh` already uses, and the only reason the endpoint trial above was safe.

Wiring the UART needs no configuration change: `console=ttyS0,115200` is already on the kernel
command line and `serial-getty@ttyS0` is already running. Three jumpers to a 3.3 V USB-serial cable,
with its 5 V left disconnected:

| Cable | Pi header |
|---|---|
| GND | pin 6 |
| RX  | pin 8  (GPIO14, Pi TX) |
| TX  | pin 10 (GPIO15, Pi RX) |

(The Zero 2 W ships with no header fitted, so this may be a soldering job.)

Device-tree routing is confirmed correct on this unit — `serial0 -> /soc/serial@7e215040` is the
mini-UART carrying the console, `serial1 -> /soc/serial@7e201000` is the PL011 held by Bluetooth.
`/dev/ttyAMA0` is deliberately absent: `hci_uart_bcm` binds the PL011 as a **serdev**, so there is no
tty node to find. `hci0` reports `B8:27:EB:D5:04:C7` — a real BD address means the firmware patchram
loaded, which is the positive confirmation that the `miniuart-bt` trap was avoided.

> [!WARNING]
> **Never add `dtoverlay=miniuart-bt`.** On this SoC family it leaves a `bluetooth` node on both
> UARTs, `hci_uart_bcm` binds the PL011 that the overlay just routed to the GPIO header, and you
> lose the serial console *and* Bluetooth together. Two of the three channels above, from one line.
> `enable_uart=1` alone is the correct setting: console on the mini-UART, BT stays on PL011.

## Usage

```sh
pizero/tools/pizero_flash.sh --disk diskN          # fetch, verify, write, customise the card
#   ... boot the Pi with the OTG ("USB") port cabled to this Mac ...
pizero/tools/pizero_link.sh                        # bring up the Mac side of the link, find the Pi
pizero/tools/pizero_gadget_install.sh              # replace g_ether with the configfs NCM+ACM gadget
pizero/tools/pizero_verify.sh                      # the hardware report -> pizero/evidence/
```

`pizero_flash.sh` writes only to a removable disk, refuses one that already carries a Linux or
Android layout unless forced, and prints the partition table it is about to destroy first. The two
other SD cards on this bench (the AAOS Pi 4 and Pi 5 images) are exactly what that guard is for.
