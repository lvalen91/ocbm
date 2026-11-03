#!/bin/sh
# ccpa_custom / Lever 1 — earliest-possible UART root console.
# Registered as a ::sysinit action BEFORE /etc/init.d/rcS so a diagnostic root shell
# exists on ttymxc0 (115200 8N1) before rcS's mount/mdev/service steps can hang.
# Backgrounds a self-respawning login shell and returns IMMEDIATELY (sysinit must not block).
{
  while true; do
    stty -F /dev/ttymxc0 115200 2>/dev/null
    setsid /bin/sh -l 0<>/dev/ttymxc0 1>&0 2>&0
    sleep 0.3
  done
} </dev/null >/dev/null 2>&1 &
exit 0
