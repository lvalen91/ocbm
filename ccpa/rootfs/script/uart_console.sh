#!/bin/sh
# Persistent 115200 8N1 serial console on ttymxc0 (i.MX6UL UART1 / board pads TX1/RX1).
# busybox init opens /dev/ttymxc0 as this process's controlling tty (stdin/stdout/stderr),
# so set the baud here BEFORE the shell prints anything -> the very first prompt is 115200.
stty 115200 2>/dev/null
[ -r /etc/profile ] && . /etc/profile
exec /bin/sh
