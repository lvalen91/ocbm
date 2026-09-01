#!/bin/bash
# Install staged binaries onto the box from a FAT32 USB stick, driven over the UART.
#
# WHY: uart_push.sh streams the whole binary as base64 through a 115200 console — ~25 s per binary at
# best, and it stalls outright if anything else holds the port. Staging on a stick puts only short
# COMMANDS on the UART and lets the box's own USB host read the bytes. The adapter stays plugged into
# the head unit, so this costs no ADB and no truck downtime.
#
# PREREQUISITE: the stick is FAT32 (`msdos`), not exFAT — the box's kernel has vfat, not exfat.
# FAT carries no permission bits, so every file lands 0644-ish and MUST be chmod'd after the copy.
#
# THE RUNNING-BINARY TRAP: `cp` over a running executable fails with ETXTBSY. We copy to a `.new`
# beside the target (same filesystem) and `mv` it into place — rename is atomic and succeeds even
# while the old inode is still executing. The running process keeps the old inode until restarted.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
UART="$REPO/host/uart_cmd.sh"
OUT="${OUT:-/tmp/usb_install.txt}"

say() { echo "[usb-install] $*"; }

# Run one command on the box and echo the captured output.
box() { bash "$UART" "$OUT" "${2:-14}" "$1" 2>&1 | tail -25; }

say "1/4 mounting the stick"
box 'mkdir -p /mnt/usb; for d in /dev/sda1 /dev/sda /dev/sdb1 /dev/sdb; do
       [ -b "$d" ] && mount -t vfat "$d" /mnt/usb 2>/dev/null && echo "MOUNTED $d" && break
     done; ls -la /mnt/usb 2>/dev/null | head' 20

say "2/4 checksums as the box sees them (must match the host md5s)"
box 'md5sum /mnt/usb/carplay-wireless /mnt/usb/ocbmd 2>&1' 30

say "3/4 installing via copy-then-rename"
box 'set -e
     for b in carplay-wireless ocbmd; do
       cp /mnt/usb/$b /usr/sbin/$b.new && chmod 755 /usr/sbin/$b.new && mv /usr/sbin/$b.new /usr/sbin/$b \
         && echo "INSTALLED $b"
     done
     md5sum /usr/sbin/carplay-wireless /usr/sbin/ocbmd; df -h / | tail -1' 40

say "4/4 unmounting"
box 'umount /mnt/usb && echo UNMOUNTED; ls -la /usr/sbin/carplay-wireless /usr/sbin/ocbmd' 20

cat <<'EOF'
[usb-install] done. Compare the md5s above against the host:
    carplay-wireless  e926c7c41b2b96cf7983234ed1c8a1b9
    ocbmd             98dd66a71f6567637c9cdc5cdf3f7425
Restart the services on the box only after BOTH match:
    killall ocbmd carplay-wireless
EOF
