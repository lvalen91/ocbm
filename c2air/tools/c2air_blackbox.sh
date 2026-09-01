#!/bin/sh
# Compact pre-crash recorder. The box has reset three times during wireless activity and the kernel
# keeps NOTHING across a reset (no pstore, no /proc/last_kmsg, no watchdog bootstatus), so the only
# way to see the state leading up to it is to persist it ourselves.
# Ring-buffered to UDISK: ~300 KB free there and it holds the per-unit carplay.key, so this keeps
# only the last MAX lines and rewrites in place rather than appending forever.
OUT=/mnt/UDISK/blackbox.log
MAX=120
: > $OUT
while true; do
  UP=$(cut -d' ' -f1 /proc/uptime)
  MF=$(awk '/MemFree/{print $2}' /proc/meminfo)
  MA=$(awk '/MemAvailable/{print $2}' /proc/meminfo)
  # biggest RSS consumer, to catch a runaway before an OOM
  TOP=$(ps -o pid,rss,comm 2>/dev/null | sort -rn -k2 | head -2 | tail -1 | tr -s ' ')
  N=$(ps | grep -c '[/]tmp/')
  echo "$UP free=${MF}k avail=${MA}k procs=$N top=[$TOP]" >> $OUT
  # trim in place
  L=$(wc -l < $OUT)
  [ "$L" -gt "$MAX" ] && { tail -n $MAX $OUT > $OUT.t && mv $OUT.t $OUT; }
  sync
  sleep 5
done
