#!/bin/sh
# peer_store.sh — the SANCTIONED, idle-gated mediator for the persistent pairing store
# /etc/carplay_peers.bin. Task #25 / docs/carplay/02_SESSION_LIFECYCLE.md.
#
# The docs/carplay/02_SESSION_LIFECYCLE.md stall was triggered by deleting this file WHILE A SESSION WAS LIVE: airplayd reads the
# store only once at startup, so a live `rm` silently diverges disk from the in-memory map and detonates
# at an arbitrary supervisor-chosen restart. This tool refuses to mutate the store while a host is
# present (present==1). Pairing changes therefore only ever happen at an idle boundary followed by a
# clean airplayd (re)start — never live. Use --defer to queue a mutation the supervisor applies at the
# next idle (host-GONE) edge.
#
#   peer_store.sh list                 # show the store (read-only, always allowed)
#   peer_store.sh path                 # print the store path
#   peer_store.sh clear [--defer]      # forget all devices (forces cold pair-setup on next connect)
#
# DO NOT `rm`/edit /etc/carplay_peers.bin directly on a live box — that is the docs/carplay/02_SESSION_LIFECYCLE.md trigger.
set -u
export PATH=/usr/sbin:/usr/bin:/sbin:/bin:$PATH
STORE=/etc/carplay_peers.bin
PRESENT=/tmp/host_present
PENDING=/tmp/peer_pending

present() { [ "$(cat "$PRESENT" 2>/dev/null)" = 1 ]; }

do_clear() {   # actual mutation; caller must have ensured we are idle
  rm -f "$STORE" 2>/dev/null
  sync
  echo "[peer] cleared $STORE — cold pair-setup on next connect"
}

cmd=${1:-}; opt=${2:-}
case "$cmd" in
  list)
    if [ -f "$STORE" ]; then
      echo "[peer] $STORE: $(wc -c < "$STORE" 2>/dev/null) bytes"
    else
      echo "[peer] $STORE: empty/absent"
    fi
    ;;
  path)
    echo "$STORE"
    ;;
  clear)
    if present; then
      if [ "$opt" = "--defer" ]; then
        printf 'clear' > "$PENDING.tmp" 2>/dev/null && mv "$PENDING.tmp" "$PENDING" 2>/dev/null && sync
        echo "[peer] host PRESENT — DEFERRED: supervisor will clear at the next idle boundary"
      else
        echo "[peer] REFUSED: host present (session live)." >&2
        echo "[peer] Stop the host app first (present->0), or pass --defer to queue it for idle." >&2
        echo "[peer] (Mutating the store live desyncs disk<->memory — the docs/carplay/02_SESSION_LIFECYCLE.md stall.)" >&2
        exit 1
      fi
    else
      do_clear
    fi
    ;;
  *)
    echo "usage: peer_store.sh {list | path | clear [--defer]}" >&2
    exit 2
    ;;
esac
