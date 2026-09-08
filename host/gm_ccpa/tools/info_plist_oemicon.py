#!/usr/bin/env python3
"""Assert the OEM-icon HIDE state in the served /info plist.

WHY THIS EXISTS
---------------
`/info` for this app is a STATIC binary plist baked into the APK
(`netprobe_app/app/src/main/assets/info.bplist`), served by the Rust core to the iPhone. It is NOT
generated from the `CT_SUBSCRIBE` YAML: `oemIconConfig` is read by airplayd's `load_device_config`,
and this app never runs airplayd (see native/carplay-jni/Cargo.toml, "LEVERS"). So the YAML cannot
turn the OEM icon off here — only this file can.

And OMITTING the keys is NOT the same as turning the icon off. From the macOS host's model
(`ccpa_custom/host/CarPlayHost/carlink_macOS/App/Settings/VehicleProfile.swift:299-303`):

    `visible`: Apple's `oemIconVisible` -- sending `visible: false` WITH the icon present is the
    active hide signal iOS honours; omitting the block leaves the cached icon on screen.

That is exactly the symptom this fixes: the plist carried no `oemIcon*` keys at all, so iOS kept
showing a cached icon on the CarPlay home screen. The hide signal requires the icon to be PRESENT
and `oemIconVisible` to be FALSE.

WHAT IT WRITES
--------------
Mirrors `receiver::info` byte-for-byte (`ccpa_custom/crates/vendor/receiver/src/info.rs:1267-1282`):

    oemIcons        array of dicts, one per resolution:
                      imageData    Data   (PNG)
                      widthPixels  Integer
                      heightPixels Integer
                      prerendered  Boolean true
    oemIconVisible  Boolean false

Three sizes, not one: Apple's AppStub emits 120/180/256 "for each required size", and a single-size
set makes iOS render the LABEL but not the image (device-confirmed 2026-08-02,
`vehicle_config.rs:108-110`). We want neither rendered, but we match the shape iOS expects rather
than relying on it tolerating a short set.

`oemIconLabel` is deliberately NOT written -- upstream only emits it when non-empty, and a hidden
icon has nothing to label.

The images are fully transparent PNGs. The icon exists only to make `visible: false` legal; if any
consumer ever ignores the flag, a transparent image is the safe failure.

Idempotent: run it as many times as you like. Verifies its own output before overwriting.

    python3 tools/info_plist_oemicon.py            # apply (and report)
    python3 tools/info_plist_oemicon.py --check    # exit 1 if the hide state is not present
"""

import plistlib
import struct
import sys
import zlib
from pathlib import Path

PLIST = Path(__file__).resolve().parent.parent / "netprobe_app/app/src/main/assets/info.bplist"

# Apple's AppStub sizes. See module docstring.
SIZES = (120, 180, 256)


def transparent_png(n: int) -> bytes:
    """A fully transparent n x n RGBA PNG, hand-rolled so this script needs no image library.

    Pure stdlib on purpose: tools/ is run on a bench Mac with whatever Python is present, and a
    Pillow dependency for four hundred bytes of zeroes would be a poor trade.
    """
    def chunk(tag: bytes, data: bytes) -> bytes:
        return (struct.pack(">I", len(data)) + tag + data
                + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF))

    # bit depth 8, colour type 6 (RGBA), deflate, no filter, no interlace.
    ihdr = struct.pack(">IIBBBBB", n, n, 8, 6, 0, 0, 0)
    # Each scanline is prefixed with filter type 0, then n pixels of RGBA(0,0,0,0).
    raw = b"".join(b"\x00" + b"\x00" * (n * 4) for _ in range(n))
    return (b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", ihdr)
            + chunk(b"IDAT", zlib.compress(raw, 9))
            + chunk(b"IEND", b""))


def hide_block() -> dict:
    return {
        "oemIcons": [
            {
                "imageData": transparent_png(n),
                "widthPixels": n,
                "heightPixels": n,
                "prerendered": True,
            }
            for n in SIZES
        ],
        "oemIconVisible": False,
    }


def is_hidden(d: dict) -> bool:
    """The hide state as iOS reads it: icons present AND visible false."""
    icons = d.get("oemIcons")
    return bool(icons) and d.get("oemIconVisible") is False


def main() -> int:
    check_only = "--check" in sys.argv
    if not PLIST.exists():
        print(f"FATAL: {PLIST} not found", file=sys.stderr)
        return 2

    with PLIST.open("rb") as f:
        d = plistlib.load(f)

    if check_only:
        ok = is_hidden(d)
        print(f"{'OK' if ok else 'MISSING'} — oemIcons={len(d.get('oemIcons') or [])} "
              f"oemIconVisible={d.get('oemIconVisible')!r}")
        return 0 if ok else 1

    before = PLIST.stat().st_size
    d.update(hide_block())
    # FMT_BINARY: the receiver serves these bytes verbatim as `application/x-apple-binary-plist`.
    with PLIST.open("wb") as f:
        plistlib.dump(d, f, fmt=plistlib.FMT_BINARY, sort_keys=True)

    with PLIST.open("rb") as f:
        back = plistlib.load(f)
    if not is_hidden(back):
        print("FATAL: wrote the plist but the hide state did not take", file=sys.stderr)
        return 2

    sizes = [(i["widthPixels"], len(i["imageData"])) for i in back["oemIcons"]]
    print(f"OK — {PLIST.name}: {before} -> {PLIST.stat().st_size} bytes")
    print(f"     oemIconVisible=False, oemIcons={sizes} (px, PNG bytes)")
    print(f"     keys: {', '.join(sorted(back))}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
