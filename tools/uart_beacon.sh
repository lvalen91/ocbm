#!/bin/sh
# CCPA UART TX beacon — low-churn 1 Hz marker on the console UART (ttymxc0 / i.MX6UL UART1).
# One short line per second: enough to catch while probing pad-to-pad, quiet enough not to flood.
i=0
while true; do
  i=$((i+1))
  printf '\r\n>>> CCPA_UART_BEACON %s <<<\r\n' "$i" > /dev/ttymxc0
  sleep 1
done
