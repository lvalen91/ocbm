#!/bin/bash
# Live adapter console stream. Shared by drive_capture.sh and usable standalone.
#
# Re-arms every REARM seconds with `tail -n 2`, not `-n 0`, ON PURPOSE: over a long unattended
# capture an idle box and a dead leg look identical, so each re-arm replays the last 2 lines of
# each file as a proof-of-life and a context anchor. Ctrl-C precedes each re-arm so box-side tails
# cannot pile up.
set -uo pipefail
PORT=/dev/cu.usbserial-0001
OUT="${1:?outdir}"
REARM="${2:-300}"
FILES='/tmp/wl.log /tmp/supervisor.log /tmp/ocbmd.log'
while :; do
    exec 3<>"$PORT" 2>/dev/null || { sleep 5; continue; }
    # OPEN FIRST, THEN stty — opening a macOS serial device resets termios (uart_cmd.sh rule 1).
    stty -f "$PORT" 115200 clocal -echo -icanon min 1 time 0 ixon 2>/dev/null
    cat <&3 >> "$OUT/adapter_console.log" 2>/dev/null & CP=$!
    echo $CP > "$OUT/.pid_uart_cat"
    sleep 0.5
    # A previous run may have left a box-side `tail -F` owning the console. Anything we type then
    # goes to THAT process's stdin and is silently swallowed — the console never reaches a prompt
    # and the leg looks alive while capturing nothing. Always Ctrl-C into a clean prompt first.
    printf '\003' >&3 2>/dev/null; sleep 0.5
    printf '\003' >&3 2>/dev/null; sleep 0.5
    while kill -0 $CP 2>/dev/null; do
        printf '\r\n' >&3 2>/dev/null
        printf '### uart re-arm %s\r\n' "$(date -u '+%FT%TZ')" >&3 2>/dev/null
        printf 'tail -n 2 -F %s\r\n' "$FILES" >&3 2>/dev/null
        for _ in $(seq 1 "$REARM"); do kill -0 $CP 2>/dev/null || break; sleep 1; done
        printf '\003' >&3 2>/dev/null
        sleep 1
    done
    exec 3>&- 2>/dev/null
    echo "### uart ended $(date -u '+%FT%TZ') — restarting" >> "$OUT/adapter_console.log"
    sleep 5
done
