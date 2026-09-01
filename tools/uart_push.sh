#!/bin/bash
# uart_push.sh LOCAL REMOTE [mode] — deploy a file to the CCPA over the UART root console when OCBM is
# unavailable (app closed → accessory de-enumerated). gzip locally, stream base64 over the serial console
# with XON/XOFF flow control, decode+gunzip on the box, verify md5 end-to-end. Slow but reliable.
set -u
PORT=/dev/cu.usbserial-0001
LOCAL="$1"; REMOTE="$2"; MODE="${3:-755}"
[ -f "$LOCAL" ] || { echo "no such file: $LOCAL"; exit 1; }

WANT_MD5=$(md5 -q "$LOCAL")
GZ=$(mktemp -t up).gz; B64=$(mktemp -t up).b64
gzip -9 -c "$LOCAL" > "$GZ"
base64 < "$GZ" | fold -w 120 > "$B64"      # wrapped lines keep the box tty's canonical buffer happy
echo "[push] $LOCAL ($(stat -f%z "$LOCAL") B) -> $REMOTE  gz=$(stat -f%z "$GZ") b64=$(stat -f%z "$B64")  md5=$WANT_MD5"

CAP=$(mktemp -t upcap)
exec 3<>"$PORT" || { echo "open $PORT failed"; exit 1; }
# Mac side: honor XOFF/XON from the box (ixon), no local echo, byte-at-a-time read.
stty -f "$PORT" 115200 clocal -echo -icanon min 1 time 0 ixon 2>/dev/null
cat <&3 > "$CAP" 2>/dev/null & CP=$!
sleep 0.4
printf '\r\n' >&3; sleep 0.3

# Box: canonical mode (Ctrl-D EOF works), echo OFF (don't reflect 600 KB back), ixoff (throttle the Mac).
printf 'stty -echo ixoff; base64 -d > %s.gz\r\n' "$REMOTE" >&3
sleep 0.6
# Stream the base64 body; the kernel tty layer pauses on the box's XOFF.
cat "$B64" >&3
sleep 0.5
printf '\n\004' >&3            # newline + Ctrl-D → EOF to base64 -d
sleep 1.0
# Decode + install + verify on the box.
printf 'stty echo; gunzip -c %s.gz > %s && chmod %s %s && rm -f %s.gz && echo PUSHMD5=$(md5sum %s | cut -d" " -f1)\r\n' \
       "$REMOTE" "$REMOTE" "$MODE" "$REMOTE" "$REMOTE" "$REMOTE" >&3
sleep 3.0

kill "$CP" 2>/dev/null; wait "$CP" 2>/dev/null
exec 3>&- 2>/dev/null
GOT=$(tr -cd '[:print:]\n' < "$CAP" | sed -n 's/.*PUSHMD5=\([0-9a-f]\{32\}\).*/\1/p' | tail -1)
rm -f "$GZ" "$B64" "$CAP"
if [ "$GOT" = "$WANT_MD5" ]; then echo "[push] OK  md5 match ($GOT)"; exit 0
else echo "[push] MD5 MISMATCH  want=$WANT_MD5 got=${GOT:-<none>}"; exit 2; fi
