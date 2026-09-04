# USB Reset / iPhone Re-enumeration Investigation — 2026-07-10

Hardware investigation (on the live CCPA over UART) into whether a wedged iPhone can be recovered by a
box-side USB reset SHORT of a full box reboot. Motivated by the #24 finding that `phone_reset` (OTG
baseline) does not recover a deeply-wedged iPhone, and that the docs/carplay/02_SESSION_LIFECYCLE.md stall needed a power cycle.

**Bottom line:** the phone-facing controller can be reset at every level, and VBUS 5V is programmatically
switchable — but **no box-side reset re-enumerates a wedged iPhone; only a full box reboot does.** The
blocker is on the iPhone side, not the box.

## The reset ladder available (phone side = `ci_hdrc.0`; OCBM = `ci_hdrc.1`, never touched)
All exposed and confirmed present; strictly scope any reset to the phone side.
| Level | Mechanism | sysfs |
|---|---|---|
| 1. VBUS flip | OTG `a_bus_drop` (1=off, 0=on) + `a_bus_req` | `/sys/bus/platform/devices/ci_hdrc.0/inputs/` |
| 2. OTG bus reset | `a_reset_req_inf` | same (untested) |
| 3. Controller restart | unbind/rebind `ci_hdrc.0` | `/sys/bus/platform/drivers/ci_hdrc/{unbind,bind}` |
| 4. Glue+PHY restart | unbind/rebind `2184000.usb` | `/sys/bus/platform/drivers/imx_usb/{unbind,bind}` |
| 5. PHY | `mxs_phy` / `20c9000.usbphy` | (untested) |
| 6. Box reboot | full re-init | `reboot` |

Layout: `2184000.usb`→`ci_hdrc.0` (phone), `2184200.usb`→`ci_hdrc.1` (OCBM/Mac); PHYs `20c9000`/`20ca000`;
`2184800.usbmisc`; `usb_vbus_wakeup.0`. No external VBUS regulator/GPIO — VBUS is controller-gated.

## VBUS 5V control — CONFIRMED programmatically switchable
- `echo 1 > .../inputs/a_bus_drop` → OTG `a_wait_vfall → a_idle` = **VBUS 5V OFF**; iPhone disconnects
  (gadget `CONFIGURED→DISCONNECTED`, `ncm0` down, `gether_disconnect`).
- `echo 0 > a_bus_drop; echo 1 > a_bus_req` → `a_wait_vrise → a_wait_bcon` = **VBUS 5V ON**, A-host waiting
  for the iPhone to connect.

## Test results — none re-enumerated the iPhone
| Reset attempted | iPhone returns as USB device (`05ac`)? |
|---|---|
| VBUS off/on — 3 s, 6 s, **20 s** | ❌ stays `a_wait_bcon` |
| Controller unbind/rebind (`ci_hdrc.0`) | ❌ controller re-inits, still `a_wait_bcon` (OCBM intact) |
| Glue+PHY rebind (`2184000.usb`) | ❌ `ci_hdrc.0` torn down + recreated, OCBM intact, still `05ac=0` |
| VBUS pulse + `projection_up` | ❌ `projection_up` fails at its `05ac` gate |
| **Box reboot** | ✅ recovers every time (3× today) |

## Why — physical disconnect vs box-side reset
Every box-side reset leaves the controller in `a_wait_bcon` (A-host powering VBUS, waiting for the iPhone
to connect as a B-device) and the iPhone never connects. The iPhone was the USB **host** during projection
and does not revert to **device** mode in response to any box-side electrical reset while the cable stays
connected. A physical unplug reads as a full cable removal (VBUS + data lines gone) → the iPhone resets its
role and comes back as a device. A box reboot also recovers (not a physical unplug) — the exact
differentiator wasn't isolated (duration up to 20 s and PHY/glue re-init both ruled out), so it is likely
an iPhone-side role/timeout behavior triggered by the box being wholly absent during the reboot.

## Apple-protocol angle (why in-band commands don't help a hung state)
- **`iap_role_switch` (Apple USB vendor request `0x51`)** performs the standard device→host role switch; it is a control request sent while the iPhone is still enumerated as a USB device — unusable when `05ac` is absent.
- **Accessory-initiated teardown** (RTSP / `AirPlayReceiverSessionTearDown`) needs a live control channel.
- **Negative result:** merely dropping the control connection (killing `airplayd`) does NOT make the iPhone
  revert — it keeps the host role. A clean revert needs an explicit protocol "exit accessory mode," not a
  socket close.
- So in a fully hung wedge (iPhone not a device + control channel dead) there is no reachable in-band Apple
  command; reboot is the only recovery.

## Implication for the recovery design (#24 / #28)
- `phone_reset` (L1, OTG baseline) remains useful for **transient / cold-start** projection failures where
  the iPhone is still a device.
- The **deep "iPhone stuck as host"** wedge is recoverable ONLY by a box reboot → **L3 reboot (#28) is
  required and reliable**, not optional.
- VBUS control and controller/glue resets are real, OCBM-safe capabilities but add no recovery value for
  the deep wedge, so they are NOT being added to `phone_reset`. (VBUS `a_bus_drop` is documented here in
  case it's useful for other purposes, e.g. intentionally pausing charge output to the connected iPhone.)
- The deep wedge is largely an artifact of violent mid-session disruption (matches docs/carplay/02_SESSION_LIFECYCLE.md). Normal
  transient failures should be lighter and L1-recoverable; L3 is the backstop.
