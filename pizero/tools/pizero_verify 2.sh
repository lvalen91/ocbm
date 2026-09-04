#!/bin/bash
# pizero_verify.sh — run ON the Pi Zero 2 W. Measures every board fact the OCBM port depends on
# and prints one report. No OCBM code is involved; this is what decides whether writing any is
# worthwhile, and which of pizero/README.md's four divergences are real on this unit.
#
#   ssh zeno@192.168.51.2 'sudo pizero_verify.sh' | tee pizero/evidence/verify_$(date +%F).txt
#
# Sections map 1:1 onto the divergence table in pizero/README.md.
# Each check prints PASS / FAIL / INFO. FAIL means an OCBM assumption does not hold on this board.

PASS=0; FAIL=0
ok()   { echo "  PASS  $*"; PASS=$((PASS+1)); }
bad()  { echo "  FAIL  $*"; FAIL=$((FAIL+1)); }
info() { echo "  ....  $*"; }
hdr()  { echo; echo "== $* =============================================================" | cut -c1-78; }

echo "pizero_verify — $(date -Is)"

hdr "1  board identity"
info "model:    $(tr -d '\0' < /proc/device-tree/model 2>/dev/null)"
info "revision: $(sed -n 's/^Revision\s*:\s*//p' /proc/cpuinfo)"
info "serial:   $(sed -n 's/^Serial\s*:\s*//p' /proc/cpuinfo)"
info "kernel:   $(uname -srm)"
info "os:       $(sed -n 's/^PRETTY_NAME="\(.*\)"/\1/p' /etc/os-release)"
info "cpus:     $(nproc) x $(sed -n 's/^model name\s*:\s*//p' /proc/cpuinfo | head -1)$(sed -n 's/^CPU part\s*:\s*//p' /proc/cpuinfo | head -1)"
info "memory:   $(awk '/MemTotal/{printf "%d MB total", $2/1024} /MemAvailable/{printf ", %d MB available", $2/1024}' /proc/meminfo)"
info "rootfs:   $(df -h / | awk 'NR==2{print $3" used of "$2}')"
if tr -d '\0' < /proc/device-tree/model 2>/dev/null | grep -q "Zero 2"; then
    ok "this is a Pi Zero 2 W"
else
    bad "not a Pi Zero 2 W — the rest of this report is about a different board"
fi

hdr "2  USB gadget — the OCBM transport substrate"
UDCS=$(ls /sys/class/udc 2>/dev/null)
if [ -n "$UDCS" ]; then
    ok "UDC present: $(echo "$UDCS" | tr '\n' ' ')"
    for u in $UDCS; do
        info "$u: state=$(cat "/sys/class/udc/$u/state" 2>/dev/null) speed=$(cat "/sys/class/udc/$u/current_speed" 2>/dev/null) maxep=$(cat "/sys/class/udc/$u/a_alt_hnp_support" 2>/dev/null >/dev/null; cat "/sys/class/udc/$u/maximum_speed" 2>/dev/null)"
    done
    # The CCPA and C2Air both read /sys/class/android_usb*/android0/state. That node does not exist
    # here; anything ported has to read /sys/class/udc/<udc>/state instead.
    if [ -e /sys/class/android_usb/android0/state ]; then
        info "android_usb node also present (unexpected on mainline)"
    else
        info "no /sys/class/android_usb — ccpa/iap2d/src/main.rs:252 and c2air's poll loop both"
        info "need a /sys/class/udc/<udc>/state backend for this board"
    fi
else
    bad "no UDC — dwc2 is not in peripheral mode (config.txt: dtoverlay=dwc2,dr_mode=peripheral)"
fi

CFG=/sys/kernel/config/usb_gadget
if mountpoint -q /sys/kernel/config 2>/dev/null || [ -d "$CFG" ]; then
    ok "configfs gadget interface mounted"
else
    bad "configfs not mounted — libcomposite missing?"
fi

hdr "3  gadget functions available in this kernel"
# Which functions exist is a kernel-build fact and cannot be discovered from configfs directly,
# so probe by attempting to create each under a scratch gadget.
S=$CFG/probe
mkdir -p "$S" 2>/dev/null
for f in ncm ecm acm ffs rndis eem mass_storage hid; do
    if mkdir -p "$S/functions/$f.probe" 2>/dev/null; then
        rmdir "$S/functions/$f.probe" 2>/dev/null
        case $f in
            ffs) ok  "usb_f_$f — functionfs present: OCBM's class-0xFF bulk pair is reachable" ;;
            ncm) ok  "usb_f_$f — control link" ;;
            *)   ok  "usb_f_$f" ;;
        esac
    else
        case $f in
            ffs) bad "usb_f_ffs ABSENT — no way to present an OCBM bulk interface on this kernel" ;;
            ncm) bad "usb_f_ncm ABSENT — control link falls back to ECM" ;;
            *)   info "usb_f_$f absent" ;;
        esac
    fi
done
rmdir "$S/functions" "$S" 2>/dev/null

hdr "4  the live gadget"
G=$CFG/pizero
if [ -d "$G" ]; then
    info "bound to: $(cat "$G/UDC" 2>/dev/null || echo '(unbound)')"
    info "id:       $(cat "$G/idVendor") / $(cat "$G/idProduct")"
    info "config:   $(ls "$G/configs/c.1/" 2>/dev/null | grep '\.' | tr '\n' ' ')"
    [ -e /sys/class/net/usb0 ] && ok "usb0: $(ip -br addr show usb0)" || bad "usb0 absent"
    [ -e /dev/ttyGS0 ] && ok "ttyGS0 present — USB serial console channel" || info "no ttyGS0"
    # Both at once is the thing the CCPA's monolithic gadget could not do (docs/carplay/00_ARCHITECTURE.md).
    if [ -e /sys/class/net/usb0 ] && [ -e /dev/ttyGS0 ]; then
        ok "composite NCM+ACM live simultaneously — unlike the CCPA, adding OCBM need not"
        ok "displace the control channel"
    fi
else
    bad "no pizero gadget — is pizero-gadget.service running?"
fi

hdr "4b  endpoint budget — how many gadget functions actually fit"
# One controller means one endpoint pool, shared by every function in the config. dwc2 on BCM283x
# reports its count at probe. NCM needs 2 bulk + 1 interrupt, ACM the same, and an OCBM functionfs
# interface 2 bulk — 8 in total, which is exactly what this core has if it reports 8. This is the
# next constraint after the controller count, and it decides whether OCBM can coexist with the
# management channel or has to displace it.
EPLINE=$(dmesg 2>/dev/null | grep -m1 -oE "dwc2 [^:]+: EPs: [0-9]+.*")
if [ -n "$EPLINE" ]; then
    info "$EPLINE"
    NEP=$(echo "$EPLINE" | sed -n 's/.*EPs: \([0-9]*\).*/\1/p')
    info "in use now: $(ls -d /sys/kernel/config/usb_gadget/pizero/configs/c.1/*.* 2>/dev/null | wc -l | tr -d ' ') function(s)"
    # MEASURED 2026-08-22 on this board, not reasoned about: NCM(3 eps) + ACM(3 eps) + a bulk pair
    # (mass_storage, the same endpoint shape as an OCBM functionfs interface) bound together at
    # high speed with usb0 and ttyGS0 both live. So "EPs: 8" is not one pool of eight — dwc2 keeps
    # separate eps_in[]/eps_out[] arrays, and the practical limit is TX FIFO space in SPRAM, not
    # the count. An earlier revision of this script called an 8-endpoint config "exactly full";
    # that was wrong and the trial is what corrected it.
    if [ "${NEP:-0}" -ge 8 ]; then
        ok "$NEP endpoints — a 3-function/8-endpoint composite is proven to bind on this board"
    else
        info "only ${NEP:-?} endpoints reported — fewer than the 8 measured here; re-run the"
        info "three-function bind trial before assuming OCBM can join the management channel"
    fi
else
    info "dwc2 endpoint count not in dmesg (ring buffer may have wrapped); try:"
    info "  dmesg | grep -i 'dwc2.*EPs'"
fi

hdr "5  MFi coprocessor — DEFERRED, informational only"
# Nothing is wired to GPIO2/GPIO3 yet, so this section never scores. It exists so that the day a
# coprocessor is fitted, the same report answers the question without being edited.
if [ -e /dev/i2c-1 ]; then
    info "/dev/i2c-1 present — the bus ccpa/iap2d and mfi_local.rs already target"
    if command -v i2cdetect >/dev/null; then
        info "i2cdetect -y 1:"
        i2cdetect -y 1 2>&1 | sed 's/^/        /'
        if i2cdetect -y 1 2>/dev/null | awk 'NR>1' | grep -qE '(^|[[:space:]])11([[:space:]]|$)'; then
            info "a device answers at 0x11 — candidate MFi 2.0C, no remote path needed"
        else
            info "nothing at 0x11: no local coprocessor. Sessions need CARPLAY_MFI_ADDR pointed at"
            info "a CCPA running ccpa/mfid (see pi/docs/00_PI_AAOS_PORT.md §1), or an MFi 2.0C"
            info "wired to GPIO2/GPIO3."
        fi
    else
        info "i2c-tools not installed (apt install i2c-tools) — cannot probe 0x11"
    fi
else
    info "/dev/i2c-1 absent — config.txt needs dtparam=i2c_arm=on. Not scored: no chip is fitted"
    info "yet, so this only matters once one is."
fi

hdr "6  Wi-Fi — can the internal radio host the 5 GHz CarPlay AP?"
if [ -d /sys/class/net/wlan0 ]; then
    ok "wlan0 exists — the interface name the daemons already assume"
    info "driver:  $(basename "$(readlink -f /sys/class/net/wlan0/device/driver 2>/dev/null)" 2>/dev/null)"
    info "regdom:  $(iw reg get 2>/dev/null | sed -n 's/^country //p' | head -1)"
    BANDS=$(iw phy 2>/dev/null | sed -n 's/^\tBand \([0-9]\).*/\1/p' | tr '\n' ' ')
    info "bands:   ${BANDS:-unknown}"
    if iw phy 2>/dev/null | grep -qE '^\s+\* 5[0-9]{3}(\.[0-9]+)? MHz'; then
        ok "5 GHz channels present — wireless CarPlay handoff may be possible on the internal radio"
        iw phy 2>/dev/null | grep -E '^\s+\* 5[0-9]{3}' | head -5 | sed 's/^/        /'
    else
        info "no 5 GHz channels — 2.4 GHz only, as expected of the CYW43438. NOT a blocker:"
        info "docs/wireless/00_WIRELESS_CARPLAY.md has 5 GHz as recommended, not required, and the vendor firmware ships"
        info "/etc/wifi_use_24G (forces ch 6, docs/wireless/01_BT_AND_RADIO.md:254). The real cost is the coexistence rule in"
        info "the same table: BT OFF during an active session, since the extra-BT-profile allowance"
        info "applies only to a 5 GHz AP. Survivable — BT carries the 0x5702/0x5703 handoff and then"
        info "wireless metadata rides the AirPlay DataStream (type 130, docs/carplay/05_METADATA_AND_CONTROLS.md), not BT."
    fi
    if iw phy 2>/dev/null | grep -A6 "Supported interface modes" | grep -q "\* AP"; then
        ok "AP mode supported by the driver"
    else
        bad "driver does not advertise AP mode"
    fi
else
    bad "no wlan0"
fi

hdr "7  Bluetooth"
if [ -d /sys/class/bluetooth/hci0 ]; then
    ok "hci0 present"
    info "$(hciconfig hci0 2>/dev/null | head -3 | tr '\n' ' ' | tr -s ' ')"
    # The miniuart-bt trap: BT must sit on the PL011, console on the mini-UART. If serial0 and the
    # bluetooth node have been swapped you lose the serial console AND Bluetooth together.
    for a in serial0 serial1; do
        info "aliases/$a: $(tr -d '\0' < /proc/device-tree/aliases/$a 2>/dev/null)"
    done
    if grep -q "^dtoverlay=miniuart-bt" /boot/firmware/config.txt 2>/dev/null; then
        bad "config.txt has dtoverlay=miniuart-bt — remove it (see pizero/README.md warning)"
    else
        ok "no miniuart-bt overlay"
    fi
    info "hci attach is in-kernel serdev — unlike the C2Air, no btattach equivalent is needed"
else
    bad "no hci0"
fi

hdr "8  kernel features the daemons rely on"
KC=""
[ -e /proc/config.gz ] && KC="zcat /proc/config.gz"
[ -e "/boot/config-$(uname -r)" ] && KC="cat /boot/config-$(uname -r)"
if [ -n "$KC" ]; then
    # NOT CONFIG_USB_DWC2_PERIPHERAL: PERIPHERAL / HOST / DUAL_ROLE are mutually exclusive Kconfig
    # choices, and this kernel picks DUAL_ROLE, which subsumes peripheral. Checking for PERIPHERAL
    # reported a FAIL on a board whose gadget was demonstrably enumerated at the time.
    for opt in CONFIG_BT_RFCOMM CONFIG_USB_CONFIGFS_F_FS CONFIG_USB_CONFIGFS_NCM \
               CONFIG_USB_CONFIGFS_ACM CONFIG_USB_DWC2 CONFIG_I2C_BCM2835 \
               CONFIG_CFG80211 CONFIG_MAC80211; do
        v=$($KC 2>/dev/null | sed -n "s/^$opt=//p")
        case "$v" in
            y|m) ok "$opt=$v" ;;
            *)   if [ "$opt" = CONFIG_BT_RFCOMM ]; then
                     bad "$opt not set — the AAOS port had to write rfcomm_uspace.rs for exactly this"
                 else bad "$opt not set"; fi ;;
        esac
    done
else
    info "no kernel config exposed (no /proc/config.gz, no /boot/config-*)"
fi
DR=$($KC 2>/dev/null | sed -n 's/^CONFIG_USB_DWC2_\(DUAL_ROLE\|PERIPHERAL\|HOST\)=.*/\1/p')
case "$DR" in
    DUAL_ROLE) ok "CONFIG_USB_DWC2_DUAL_ROLE — peripheral available, and host too if a phone-facing"
               info "role were ever wanted (it cannot be, on one controller — see pizero/README.md)" ;;
    PERIPHERAL) ok "CONFIG_USB_DWC2_PERIPHERAL" ;;
    HOST)      bad "dwc2 built HOST-only — this board cannot be a gadget at all" ;;
    *)         info "dwc2 role mode not determinable from the kernel config" ;;
esac
lsmod | grep -q '^bluetooth' && info "bluetooth module loaded"
modinfo rfcomm >/dev/null 2>&1 && ok "rfcomm module available — kernel RFCOMM, no userspace backend needed"

hdr "9  headroom"
info "load:    $(cut -d' ' -f1-3 /proc/loadavg)"
info "cpufreq: $(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_cur_freq 2>/dev/null | awk '{print $1/1000" MHz"}') (max $(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_max_freq 2>/dev/null | awk '{print $1/1000" MHz"}'))"
info "temp:    $(awk '{printf "%.1f C", $1/1000}' /sys/class/thermal/thermal_zone0/temp 2>/dev/null)"
info "throttle:$(vcgencmd get_throttled 2>/dev/null)"
info "memory:  $(free -m | awk 'NR==2{print $3" MB used / "$2" MB"}')"
# The box relays ChaCha20-Poly1305 A/V rather than decoding it, so bulk symmetric throughput is the
# relevant number, not video decode.
if command -v openssl >/dev/null; then
    info "chacha20-poly1305 (1 core, 8 KB blocks):"
    openssl speed -elapsed -evp chacha20-poly1305 2>/dev/null | tail -2 | sed 's/^/        /'
fi

hdr "summary"
echo "  $PASS pass, $FAIL fail"
if [ "$FAIL" -gt 0 ]; then
    echo
    echo "  A FAIL here is a design input, not necessarily a defect — pizero/README.md lists which"
    echo "  divergences were expected (no MFi chip, no 5 GHz, no f_accessory, one USB port)."
fi
exit 0
