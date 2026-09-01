#!/bin/sh
# capture_iphone_carplay.v2.sh
# Reliable capture of the iPhone's OWN iAP2 packet trace + CoreMedia carEndpoint
# logs over USB, for the ccpa_custom CarPlay accessory project.
#
# WHAT MAKES THE TRACE APPEAR (verified 2026-07-25 against the good/bad captures):
#   The `LOG; ...` iAP2 packet trace and the `iAP2PacketParseBuffer` lines are emitted
#   by accessoryd ONLY when the iapd/CarPlay debug preferences are set AND accessoryd
#   was (re)started after they were set. In every capture that had the trace, accessoryd
#   was emitting <Debug>-level lines (2543 / 3902 of them). In every capture that lacked
#   the trace, accessoryd emitted ZERO <Debug> lines. That <Debug> stream is the tell.
#
#   Preconditions (see PLAYBOOK section at bottom):
#     1. Install Apple's "CarPlay" (or "iapd") debug profile on the phone, which sets
#          com.apple.iapd        PrintIapPackets=1  LogAttachEvents=1
#          com.apple.iaptransportd AppleIDBusEventLogging=1
#          com.apple.Preferences IAPLogging=1
#          com.apple.logging     LogEAEvents=1
#        These are CFPreferences read at PROCESS START.
#     2. REBOOT the phone (or otherwise relaunch accessoryd) AFTER installing.
#     3. Start THIS capture, THEN start the CarPlay session. The trace only prints
#        during link setup (~6 s), so the capture must already be running.
#
# Usage:
#   ./capture_iphone_carplay.v2.sh [OUTDIR]
#   Run it, wait for "PRECHECK", start the wireless session, Ctrl-C when done.

OUT="${1:-$(cd "$(dirname "$0")" && pwd)}"
mkdir -p "$OUT"
TS="$(date +%Y%m%d_%H%M%S)"
LIVE="$OUT/carplay_live_$TS.txt"
ARCH_TAR="$OUT/_arch_$TS.tar"
ARCH="$OUT/carplay_$TS.logarchive"
TABLE="$OUT/iap2_trace_$TS.txt"
LOG="/usr/bin/log"

UDID="$(idevice_id -l 2>/dev/null | head -1)"
if [ -z "$UDID" ]; then
  echo "ERROR: no USB device (idevice_id -l is empty). Attach + trust the phone." >&2
  exit 1
fi
echo "Device: $UDID"

# ---- PRECHECK: is verbose accessoryd logging actually live? -----------------
# We can't force it from the host, but we CAN tell you up front whether the phone
# is currently emitting the debug stream, so you don't waste a session on silence.
# We watch a 4 s window for ANY accessoryd <Debug> line (the discriminator) or the
# iapd preference echo. If none appears, the profile/reboot step was skipped.
echo "PRECHECK: sampling accessoryd for ~4 s to confirm verbose logging is live..."
PRE="$OUT/_precheck_$TS.txt"
# idevicesyslog has no timeout; run it detached and kill it.
idevicesyslog -u "$UDID" -p 'accessoryd' --no-colors -o "$PRE" 2>/dev/null &
PREPID=$!
sleep 4
kill "$PREPID" 2>/dev/null
wait "$PREPID" 2>/dev/null
if grep -q '<Debug>' "$PRE" 2>/dev/null; then
  echo "PRECHECK OK: accessoryd is emitting <Debug> -> packet trace should appear."
else
  echo "PRECHECK WARNING: no accessoryd <Debug> seen. The trace will very likely be"
  echo "  MISSING. Confirm the CarPlay/iapd debug profile is installed AND the phone"
  echo "  was rebooted after installing it, then re-run. (Idle phones emit little from"
  echo "  accessoryd, so this can occasionally be a false alarm — but treat it as real.)"
fi
rm -f "$PRE"

# ---- LIVE CAPTURE -----------------------------------------------------------
# Process set that carries CarPlay signal. bluetoothd/CommCenter excluded (they
# bury the stream at ~9700 lines / 8 s). accessoryd word-boundary matters:
# idevicesyslog -p matches process name exactly, so "accessoryd" will NOT catch
# "audioaccessoryd" (AirPods) here -- but the post-run extractor also guards it.
echo
echo "Streaming filtered live log -> $LIVE"
echo ">>> START THE WIRELESS CARPLAY SESSION NOW.  Ctrl-C when the session is over. <<<"
idevicesyslog -u "$UDID" \
  -p 'airplayd|carkitd|accessoryd|CarPlay|wifid|mediaremoted|nowplayingd|sharingd' \
  --no-colors -o "$LIVE" || true

# ---- POST RUN: pull archive (retroactive, PERSISTED entries only) -----------
# NOTE: the archive holds only what the on-device store PERSISTED. The Notice-level
# `LOG;` trace persists; the <Debug> iAP2PacketParseBuffer lines usually do NOT
# unless debug-persist is on. The live stream above is the authoritative source for
# Debug. The archive is a retroactive safety net for when you missed the window.
echo
echo "Pulling logarchive (last 30 min) -> $ARCH"
rm -rf "$ARCH" "$ARCH_TAR"
idevicesyslog -u "$UDID" archive "$ARCH_TAR" --age-limit 1800 && {
  mkdir -p "$ARCH"
  tar -xf "$ARCH_TAR" -C "$ARCH"
  rm -f "$ARCH_TAR"
}

# ---- EXTRACT the iAP2 packet trace into a readable table --------------------
"$(dirname "$0")/extract_iap2_trace.sh" "$LIVE" > "$TABLE" 2>/dev/null || \
  sh "$(dirname "$0")/extract_iap2_trace.sh" "$LIVE" > "$TABLE"
echo
echo "Done."
echo "  live stream : $LIVE"
echo "  logarchive  : $ARCH"
echo "  iAP2 table  : $TABLE"
echo
echo "Query the archive, e.g.:"
echo "  $LOG show --archive \"$ARCH\" --style compact --info --debug \\"
echo "     --predicate 'process == \"accessoryd\"' | grep -E 'LOG;|iAP2Packet'"
echo "  $LOG show --archive \"$ARCH\" --style compact --info --debug \\"
echo "     --predicate 'process == \"airplayd\"' | grep -iE 'carEndpoint|RCS|iap|SETUP'"
