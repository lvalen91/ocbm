#!/bin/bash
# pizero_flash.sh — write and customise a Raspberry Pi OS Lite card for the Pi Zero 2 W bring-up
# board. macOS host. See pizero/README.md.
#
# Everything is configured through the FAT boot partition, because macOS cannot write ext4.
# Raspberry Pi OS Trixie ships **cloud-init** (NoCloud, dsmode local) with user-data / meta-data /
# network-config on that partition, which is what makes a full headless setup possible from here —
# the older raspberrypi-sys-mods `firstboot` + `userconf.txt` path is gone from this image
# (verified: no "raspberrypi-sys-mods/firstboot" string in 2026-06-18-raspios-trixie-arm64-lite).
#
#   pizero/tools/pizero_flash.sh --disk disk8
#   pizero/tools/pizero_flash.sh --disk disk8 --wifi-ssid MySSID --wifi-psk secret
#   pizero/tools/pizero_flash.sh --disk disk8 --customise-only    # card already written
#
# On success the Pi boots with:
#   * USB NCM at 192.168.51.2  (SSH, key auth)         <- normal path
#   * USB ACM console on /dev/ttyGS0                    <- survives a broken network config
#   * GPIO UART console, 115200, GPIO14/15              <- survives a broken gadget or boot
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
CACHE=${PIZERO_CACHE:-$HOME/.cache/pizero/images}   # NOT under ~/Documents: that is iCloud-synced
IMG_REL=raspios_lite_arm64-2026-06-19/2026-06-18-raspios-trixie-arm64-lite.img.xz
IMG_URL=https://downloads.raspberrypi.com/raspios_lite_arm64/images/$IMG_REL
IMG_XZ=$CACHE/$(basename "$IMG_REL")
IMG=${IMG_XZ%.xz}

DISK=""; USERNAME=zeno; HOSTNAME_=pizero; FORCE=0; CUSTOMISE_ONLY=0
WIFI_SSID=""; WIFI_PSK=""; WIFI_COUNTRY=US; TZ_=$(readlink /etc/localtime | sed 's|.*zoneinfo/||')
PI_IP=192.168.51.2; PI_CIDR=24

die() { echo "pizero_flash: $*" >&2; exit 1; }
say() { echo "==> $*"; }

while [ $# -gt 0 ]; do
  case $1 in
    --disk)            DISK=${2#/dev/}; shift 2 ;;
    --user)            USERNAME=$2; shift 2 ;;
    --hostname)        HOSTNAME_=$2; shift 2 ;;
    --wifi-ssid)       WIFI_SSID=$2; shift 2 ;;
    --wifi-psk)        WIFI_PSK=$2; shift 2 ;;
    --wifi-country)    WIFI_COUNTRY=$2; shift 2 ;;
    --ip)              PI_IP=$2; shift 2 ;;
    --customise-only)  CUSTOMISE_ONLY=1; shift ;;
    --force)           FORCE=1; shift ;;
    -h|--help)         sed -n '2,22p' "$0"; exit 0 ;;
    *)                 die "unknown argument: $1" ;;
  esac
done
[ -n "$DISK" ] || die "--disk diskN is required (find it with: diskutil list external)"

# ---------------------------------------------------------------- preflight
# LibreSSL's openssl (/usr/bin) has no `passwd -6`; Homebrew's OpenSSL 3 does.
OPENSSL=""
for c in /opt/homebrew/bin/openssl /opt/homebrew/opt/openssl@3/bin/openssl "$(command -v openssl || true)"; do
  [ -x "$c" ] && "$c" passwd -6 -salt aaaaaaaa x >/dev/null 2>&1 && { OPENSSL=$c; break; }
done
[ -n "$OPENSSL" ] || die "no openssl with SHA-512 crypt support (brew install openssl@3)"

SSH_KEY=""
for k in "$HOME/.ssh/id_ed25519.pub" "$HOME/.ssh/id_pizero.pub"; do
  [ -f "$k" ] && { SSH_KEY=$k; break; }
done
if [ -z "$SSH_KEY" ]; then
  say "no SSH key found — generating $HOME/.ssh/id_pizero"
  ssh-keygen -t ed25519 -N "" -C "carlink-pizero" -f "$HOME/.ssh/id_pizero" >/dev/null
  SSH_KEY=$HOME/.ssh/id_pizero.pub
fi
say "ssh key: $SSH_KEY"

printf 'password for %s on the Pi (empty = generate): ' "$USERNAME" >&2
read -rs PW; echo >&2
if [ -z "$PW" ]; then
  PW=$(LC_ALL=C tr -dc 'a-zA-Z0-9' < /dev/urandom | head -c 16)
  say "generated password: $PW   <- record this now, it is not stored"
fi
PWHASH=$("$OPENSSL" passwd -6 "$PW")

# ---------------------------------------------------------------- image
mkdir -p "$CACHE"
if [ ! -f "$IMG" ]; then
  if [ ! -f "$IMG_XZ" ]; then
    say "fetching $(basename "$IMG_XZ")"
    curl -fL --retry 3 -o "$IMG_XZ" "$IMG_URL"
    curl -fsL -o "$IMG_XZ.sha256" "$IMG_URL.sha256"
  fi
  say "verifying checksum"
  (cd "$CACHE" && shasum -a 256 -c "$(basename "$IMG_XZ").sha256") || die "checksum mismatch"
  say "decompressing"
  xz -dk -T0 "$IMG_XZ"
fi
say "image: $IMG ($(du -h "$IMG" | cut -f1))"

# ---------------------------------------------------------------- disk guard
# There are other Pi cards on this bench (AAOS rpi4 and rpi5). Destroying one of those by typo is
# the failure this section exists to prevent, so the checks are deliberately unhelpful to override.
INFO=$(diskutil info "$DISK" 2>/dev/null) || die "no such disk: $DISK"
grep -q "Device Location:.*External"   <<<"$INFO" || die "$DISK is not external — refusing"
grep -q "Removable Media:.*Removable"  <<<"$INFO" || die "$DISK is not removable media — refusing"
grep -q "Virtual:.*Yes"                <<<"$INFO" && die "$DISK is a disk image, not a card"
SIZE=$(sed -n 's/^ *Disk Size: *\(.*\) (.*/\1/p' <<<"$INFO" | head -1)

if [ "$CUSTOMISE_ONLY" -eq 0 ]; then
  LAYOUT=$(diskutil list "$DISK")
  echo; echo "$LAYOUT"; echo
  if grep -qE "Linux|Android|Apple_APFS|Apple_HFS" <<<"$LAYOUT" && [ "$FORCE" -eq 0 ]; then
    echo "!! $DISK already carries a Linux/Android/macOS layout. The AAOS Pi 4 and Pi 5 cards look"
    echo "!! exactly like this. Pass --force only if you are certain this is the blank Pi Zero card."
    die "refusing to overwrite an existing layout"
  fi
  echo "About to ERASE $DISK ($SIZE) and write $(basename "$IMG")."
  printf 'Type the disk identifier to confirm (%s): ' "$DISK"
  read -r CONFIRM
  [ "$CONFIRM" = "$DISK" ] || die "confirmation did not match — nothing written"

  say "unmounting $DISK"
  diskutil unmountDisk "/dev/$DISK"
  say "writing (a few minutes; Ctrl-T prints progress)"
  sudo dd if="$IMG" of="/dev/r$DISK" bs=4m
  sync
  say "written; waiting for the boot partition to mount"
  diskutil mountDisk "/dev/$DISK" >/dev/null 2>&1 || true
  for _ in $(seq 1 30); do [ -d /Volumes/bootfs ] && break; sleep 1; done
fi

BOOT=${PIZERO_BOOT:-/Volumes/bootfs}
[ -d "$BOOT" ] || die "boot partition not mounted at $BOOT"
[ -f "$BOOT/config.txt" ] || die "$BOOT does not look like a Raspberry Pi boot partition"

# ---------------------------------------------------------------- config.txt / cmdline.txt
say "customising $BOOT"
if ! grep -q "carlink pizero" "$BOOT/config.txt"; then
cat >> "$BOOT/config.txt" <<'CFG'

# --- carlink pizero bring-up ---------------------------------------------------
# dwc2 in PERIPHERAL mode: the single data-capable micro-USB is the control link.
# The other micro-USB ("PWR IN") has no data lines, so this board is a gadget OR a
# host, never both — see pizero/README.md.
dtoverlay=dwc2,dr_mode=peripheral

# GPIO UART console (GPIO14/15, 115200). The independent recovery channel.
# DO NOT add dtoverlay=miniuart-bt to "fix" anything: it leaves a bluetooth node on
# both UARTs, hci_uart_bcm binds the PL011 the overlay just routed to the header,
# and you lose the console AND Bluetooth together. enable_uart=1 alone is correct —
# console on the mini-UART, BT stays on the PL011.
enable_uart=1

# The MFi 2.0C coprocessor, if one is ever wired to GPIO2/GPIO3, answers at 0x11 on
# this bus — the address ccpa/iap2d and crates/vendor/wireless already target.
dtparam=i2c_arm=on

# Nothing on this board drives a display.
dtoverlay=vc4-kms-v3d,noaudio
CFG
fi

# RPi OS ships ssh.service DISABLED and gates it on sshswitch.service finding this marker file.
# cloud-init's ssh_pwauth configures sshd but does NOT enable the unit, so without this the board
# boots fine, the gadget binds, the getty comes up — and port 22 refuses. Learned the hard way.
touch "$BOOT/ssh"

# modules-load is belt and braces: if cloud-init never runs, g_ether still gives a link.
# pizero-gadget rmmods it before claiming the UDC.
if ! grep -q "modules-load=dwc2" "$BOOT/cmdline.txt"; then
  perl -pi -e 'chomp; $_ .= " modules-load=dwc2,libcomposite\n"' "$BOOT/cmdline.txt"
fi

# ---------------------------------------------------------------- cloud-init
b64() { base64 -i "$1" | tr -d '\n'; }
GADGET_B64=$(b64 "$HERE/pizero-gadget")
VERIFY_B64=$(b64 "$HERE/pizero_verify.sh")

cat > "$BOOT/meta-data" <<META
dsmode: local
instance_id: carlink-pizero-$(date +%s)
local-hostname: $HOSTNAME_
META

{
cat <<UD
#cloud-config
# Generated by pizero/tools/pizero_flash.sh — carlink Pi Zero 2 W OCBM bring-up board.

hostname: $HOSTNAME_
manage_etc_hosts: true
ssh_pwauth: true

users:
  - name: $USERNAME
    gecos: carlink
    groups: [adm, sudo, dialout, i2c, gpio, netdev, plugdev]
    shell: /bin/bash
    lock_passwd: false
    passwd: "$PWHASH"
    sudo: "ALL=(ALL) NOPASSWD:ALL"
    ssh_authorized_keys:
      - "$(cat "$SSH_KEY")"

write_files:
  - path: /usr/local/sbin/pizero-gadget
    permissions: '0755'
    encoding: b64
    content: $GADGET_B64

  - path: /usr/local/sbin/pizero_verify.sh
    permissions: '0755'
    encoding: b64
    content: $VERIFY_B64

  - path: /etc/systemd/system/pizero-gadget.service
    permissions: '0644'
    content: |
      [Unit]
      Description=carlink Pi Zero 2 W USB gadget (NCM + ACM)
      After=local-fs.target
      # Before the network stack so usb0 exists when NetworkManager enumerates.
      Before=network-pre.target
      Wants=network-pre.target

      [Service]
      Type=oneshot
      RemainAfterExit=yes
      Environment=PIZERO_IP=$PI_IP
      Environment=PIZERO_CIDR=$PI_CIDR
      ExecStart=/usr/local/sbin/pizero-gadget start
      ExecStop=/usr/local/sbin/pizero-gadget stop

      [Install]
      WantedBy=multi-user.target

  - path: /etc/NetworkManager/conf.d/99-pizero-usb0.conf
    permissions: '0644'
    content: |
      # usb0 is configured statically by pizero-gadget. Leaving it to NetworkManager
      # produces a link-local address and a race against the gadget bind.
      [keyfile]
      unmanaged-devices=interface-name:usb0

  - path: /etc/motd
    permissions: '0644'
    content: |
      carlink Pi Zero 2 W — OCBM bring-up board (pizero/ in ccpa_custom)
      NCM $PI_IP  |  ACM console /dev/ttyGS0  |  UART GPIO14/15 @115200
      Hardware report:  sudo pizero_verify.sh
      This board has no MFi coprocessor and no 5 GHz radio. See pizero/README.md.

runcmd:
  - [ systemctl, daemon-reload ]
  # Redundant with /boot/firmware/ssh + sshswitch.service, deliberately: this is the only channel
  # that does not need physical access, so it gets two independent enablers.
  - [ systemctl, enable, --now, ssh ]
  - [ systemctl, enable, --now, pizero-gadget.service ]
  - [ systemctl, enable, --now, serial-getty@ttyGS0.service ]
  - [ raspi-config, nonint, do_i2c, "0" ]
UD

if [ -n "$WIFI_SSID" ]; then
cat <<UD
  - [ raspi-config, nonint, do_wifi_country, "$WIFI_COUNTRY" ]

package_update: true
packages:
  - i2c-tools
  - iw
  - bluez
  - usbutils
UD
else
cat <<'UD'

# No Wi-Fi configured, so no package installation on first boot. Once the NCM link is up:
#   ssh <pi> 'sudo apt-get update && sudo apt-get install -y i2c-tools iw bluez usbutils'
# (share the Mac's connection over the USB link, or pass --wifi-ssid/--wifi-psk when flashing).
UD
fi

cat <<UD

timezone: $TZ_
UD
} > "$BOOT/user-data"

if [ -n "$WIFI_SSID" ]; then
cat > "$BOOT/network-config" <<NET
version: 2
wifis:
  wlan0:
    dhcp4: true
    optional: true
    access-points:
      "$WIFI_SSID":
        password: "$WIFI_PSK"
    regulatory-domain: $WIFI_COUNTRY
NET
else
cat > "$BOOT/network-config" <<'NET'
version: 2
# usb0 is deliberately absent: pizero-gadget owns it and NetworkManager is told to leave it alone.
NET
fi

sync
if [ "${PIZERO_BOOT:-}" = "" ]; then
  say "ejecting"
  diskutil eject "/dev/$DISK" >/dev/null
fi

cat <<DONE

Card ready.

  1. Put it in the Pi Zero 2 W.
  2. Cable the micro-USB marked **USB** (the inner one, next to HDMI) to this Mac.
     The one marked PWR IN has no data lines — it will power the board and nothing else.
  3. First boot takes ~90 s (cloud-init runs, then the gadget binds).
  4. pizero/tools/pizero_link.sh          # brings up the Mac side and finds it
  5. ssh $USERNAME@$PI_IP 'sudo pizero_verify.sh'

If nothing enumerates, the GPIO UART console (GPIO14 TX / GPIO15 RX / GND, 115200) shows the
whole boot including cloud-init. Check /var/log/cloud-init-output.log first.
DONE
