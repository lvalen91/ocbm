#!/bin/bash
# uart_cmd.sh — drive the CCPA serial root console from the Mac (single-fd, reliable).
# Usage: uart_cmd.sh "shell command" [read_seconds]
PORT=/dev/cu.usbserial-0001
CMD="$1"; SECS="${2:-3}"
CAP=$(mktemp /tmp/uartcap.XXXXXX)
exec 3<>"$PORT" 2>/dev/null || { echo "OPEN FAILED"; exit 1; }
stty -f "$PORT" 115200 clocal -echo raw 2>/dev/null
cat <&3 > "$CAP" 2>/dev/null &
CP=$!
sleep 0.4
printf '\r\n' >&3; sleep 0.3            # flush any partial line
printf '%s\r\n' "$CMD" >&3
sleep "$SECS"
kill "$CP" 2>/dev/null; wait "$CP" 2>/dev/null
exec 3>&- 2>/dev/null
tr -cd '[:print:]\n\t' < "$CAP"
rm -f "$CAP"
