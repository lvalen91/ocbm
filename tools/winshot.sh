#!/bin/bash
# Build-on-demand wrapper for tools/winshot.swift. The binary lives in /tmp, NOT next to the source:
# ~/Documents is iCloud-synced and a build artefact there breeds "* 2.swift" conflict copies.
#     tools/winshot.sh list <pid> | shot <pid> <out.png> | stats <png> ... | diff <a> <b> ...
set -e
SRC="$(cd "$(dirname "$0")" && pwd)/winshot.swift"
BIN=/tmp/winshot-bin/winshot
mkdir -p /tmp/winshot-bin
if [ ! -x "$BIN" ] || [ "$SRC" -nt "$BIN" ]; then
  /usr/bin/swiftc -O -o "$BIN" "$SRC" >/tmp/winshot-bin/build.log 2>&1 \
    || { echo "winshot: build failed — see /tmp/winshot-bin/build.log" >&2; head -20 /tmp/winshot-bin/build.log >&2; exit 2; }
fi
exec "$BIN" "$@"
