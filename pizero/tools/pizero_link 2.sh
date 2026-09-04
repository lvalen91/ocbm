#!/bin/bash
# pizero_link.sh — macOS side of the Pi Zero 2 W USB control link.
#
# Finds the gadget's network interface by its MAC (pizero-gadget pins the host end to
# 02:51:00:00:00:01, so this needs no guessing at enN numbering), gives it 192.168.51.1, and
# reports every channel that is up.
#
#   pizero/tools/pizero_link.sh            # configure and report
#   pizero/tools/pizero_link.sh --watch    # wait for the board to appear, then configure
set -euo pipefail

HOST_MAC=${PIZERO_HOST_MAC:-02:51:00:00:00:01}
HOST_IP=${PIZERO_HOST_IP:-192.168.51.1}
PI_IP=${PIZERO_IP:-192.168.51.2}
USERNAME=${PIZERO_USER:-zeno}
WATCH=0
[ "${1:-}" = "--watch" ] && WATCH=1

find_iface() {
  # macOS ifconfig prints the MAC with no leading zeros on each octet (2:51:0:0:0:1).
  local short; short=$(echo "$HOST_MAC" | sed 's/\b0\([0-9a-f]\)/\1/g')
  ifconfig | awk -v m1="$HOST_MAC" -v m2="$short" '
    /^[a-z0-9]+:/ { iface=substr($1,1,length($1)-1) }
    /ether/       { if ($2==m1 || $2==m2) { print iface; exit } }'
}

IFACE=""
if [ "$WATCH" -eq 1 ]; then
  echo "waiting for the gadget (first boot is ~90 s)…"
  for _ in $(seq 1 180); do IFACE=$(find_iface); [ -n "$IFACE" ] && break; sleep 1; done
else
  IFACE=$(find_iface)
fi

if [ -z "$IFACE" ]; then
  cat >&2 <<'NOPE'
No interface with the gadget MAC. In order of likelihood:

  1. The cable is in the PWR IN port. Use the inner micro-USB marked USB.
  2. The micro-USB cable is charge-only. Plenty of them are; try another.
  3. cloud-init has not finished (first boot ~90 s) — rerun with --watch.
  4. cloud-init failed. Attach the GPIO UART console (GPIO14/15, 115200) and read
     /var/log/cloud-init-output.log.

  ioreg -p IOUSB -w0 -l | grep -i 'USB Product Name' shows what did enumerate, if anything.
NOPE
  exit 1
fi

echo "gadget interface: $IFACE"

# IPv6 link-local first. The Pi's usb0 MAC is pinned, so its address is a pure EUI-64 derivation of
# it — no DHCP, no mDNS, and crucially no sudo, which matters because configuring an IPv4 address on
# a macOS interface needs root and the IPv4 static is only a convenience.
PI_LL="fe80::51:ff:fe00:2%$IFACE"
if ping6 -c1 "$PI_LL" >/dev/null 2>&1; then
  echo "link-local: $PI_LL   (works without sudo)"
else
  # Fall back to discovery in case the MAC was overridden.
  FOUND=$(ping6 -c2 -I "$IFACE" ff02::1 2>/dev/null | awk -F'[ %]' '/bytes from fe80/{print $4}' \
          | sort -u | grep -v "$(ifconfig "$IFACE" | awk '/inet6 fe80/{print $2}' | cut -d% -f1)")
  [ -n "$FOUND" ] && { PI_LL="$FOUND%$IFACE"; echo "link-local: $PI_LL (discovered)"; }
fi

CUR=$(ifconfig "$IFACE" | awk '/inet /{print $2}')
if [ "$CUR" != "$HOST_IP" ]; then
  if sudo -n true 2>/dev/null; then
    echo "setting $IFACE to $HOST_IP/24"
    sudo ifconfig "$IFACE" inet "$HOST_IP" netmask 255.255.255.0 up
  else
    echo "IPv4 $HOST_IP not set (needs sudo). Not required — use the link-local address above:"
    echo "    sudo ifconfig $IFACE inet $HOST_IP netmask 255.255.255.0 up"
  fi
fi

echo
echo "channels:"
if ping6 -c2 "$PI_LL" >/dev/null 2>&1; then
  echo "  NCM/v6  $PI_LL   up"
else
  echo "  NCM/v6  $PI_LL   NO REPLY"
fi
if ping -c2 -t2 "$PI_IP" >/dev/null 2>&1; then
  echo "  NCM/v4  $PI_IP          up"
else
  echo "  NCM/v4  $PI_IP          no route (expected until the sudo line above is run)"
fi
ACM=$(ls /dev/cu.usbmodem* 2>/dev/null | head -1 || true)
if [ -n "$ACM" ]; then
  echo "  ACM     $ACM   (screen $ACM 115200)"
else
  echo "  ACM     absent — usb_f_acm missing, or the composite did not bind"
fi
UART=$(ls /dev/cu.usbserial* 2>/dev/null | head -1 || true)
# A USB-serial cable on the Mac proves nothing about whether it is wired to the Pi's GPIO14/15.
# Report it as unconfirmed rather than as a channel; on this bench it was plugged into nothing.
[ -n "$UART" ] && echo "  UART?   $UART   cable present — confirm it is on GPIO14/15 pins 8/10/6"

echo
TARGET=""
for t in "$PI_LL" "$PI_IP"; do
  ssh -o ConnectTimeout=5 -o StrictHostKeyChecking=accept-new -o BatchMode=yes \
      -i "$HOME/.ssh/id_pizero" "$USERNAME@$t" true 2>/dev/null && { TARGET=$t; break; }
done
if [ -n "$TARGET" ]; then
  echo "ssh ok:   ssh -i ~/.ssh/id_pizero $USERNAME@$TARGET"
  echo "report:   ssh -i ~/.ssh/id_pizero $USERNAME@$TARGET 'sudo pizero_verify.sh'"
else
  echo "ssh not answering. If the port REFUSES rather than times out, the host is up and sshd is"
  echo "not enabled: RPi OS gates ssh.service on sshswitch.service finding /boot/firmware/ssh."
  echo "Fix from the ACM console ($ACM):"
  echo "    sudo systemctl enable --now ssh && sudo touch /boot/firmware/ssh"
fi

cat <<'NAT'

The Pi has no route off this link. For apt, either reflash with --wifi-ssid/--wifi-psk, or turn on
System Settings > General > Sharing > Internet Sharing (share from your uplink, to the gadget
interface above) and add a default route on the Pi:
    sudo ip route add default via 192.168.51.1 && echo 'nameserver 1.1.1.1' | sudo tee /etc/resolv.conf
NAT
