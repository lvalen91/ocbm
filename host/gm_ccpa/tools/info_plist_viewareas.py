#!/usr/bin/env python3
"""Declare a SECOND view area in the served /info, enabling CarPlay's runtime resize.

WHY
---
`/info` here is a static binary plist served verbatim by the in-process Rust core; upstream's
`receiver::info::view_areas()` never runs for this app, so this file is the only lever.

Today the display declares ONE full-bleed area with `adjacentViewAreas: []` — byte-identical to what
the genuine CCPA sent, and the shape a working session is pinned to. A second area plus a non-empty
`adjacentViewAreas` is what makes iOS render the Dock resize button and emit `/command
requestViewArea`.

GEOMETRY = GM'S OWN APP BOUNDS
------------------------------
`1416x842 @ (188,118)` on a 2400x960 panel, `initialViewArea: 0`, `adjacentViewAreas: [1]`.

This is the content box AAOS gives an ordinary app on this head unit, measured from `dumpsys window`
as `mAppBounds=Rect(189, 0 - 1605, 960)`. Read that carefully, because the obvious reading is wrong:
the 189 on the left is the LeftBar, but the right edge at 1605 comes from a SEPARATE navigation-class
inset provider 795 px wide (GM's widget pane), not from a bar. The 118 px TopCarSystemBar is
status-class and does NOT appear in `mAppBounds` at all — top stays 0 — it purely overlays. So the box
is (2400-189-795) x (960-118) = 1416x842, and the 118 comes from the overlay rather than the bounds.

The origin is moved to 188 because the parity rule below is a TEARDOWN-class violation and 189 is odd;
one pixel of overlap with the LeftBar is invisible and costs nothing.

Shrinking CarPlay into exactly that box is the point of the exercise: it is the rect a native GM app
occupies, so it is the one that leaves GM's own chrome legible around the CarPlay picture.

The previously declared rect `1600x960 @ (800,0)` is the upstream device-proven-good one
(2026-09-05); this rect was validated the same way and proven on this rig 2026-09-08.
That exact declaration is recorded device-proven on the wireless arm (2026-09-05,
`box-20260905-033634.log`, `receiver/src/info.rs:397-400`): iOS negotiated `viewAreas`, showed the
Dock button, sent `requestViewArea` per press, and the picture moved — 255 rect updates with the
CODED SIZE CONSTANT. That last detail is why no decoder change is needed: the resize is destination
bounds only.

The same night, `1416x842 @ (492,59):initial` tore the session down in the same millisecond as
RECORD's session-focus handshake, on BOTH arms (`info.rs:402-419`). Two causes, both avoided here:
  1. the rect was not contained in the panel; and
  2. `adjacentViewAreas` was a constant `[1]` regardless of which area was initial, so the starting
     area was declared adjacent to ITSELF — degenerate, since CarKit models a single
     `CARScreenInfo.adjacentViewArea` beside `currentViewArea`.
Adjacency here is DERIVED from the initial index for that reason.

RULES APPLIED (upstream `ViewArea2Rule`, severity order)
  containment in the panel -> all four values even -> positive dims -> product floor (800x480
  landscape). A zero dimension trips iOS's "Pixel display view dimension(s) set to 0" validator,
  which is a teardown rather than a warning.

`safeArea` is in PANEL coordinates, like the area itself (Apple's Widescreen template puts an area at
originX 640 next to a safeArea at originX 640). Each area's safeArea is set full-bleed to its own
rect: a wrong safe rect is the "safeArea exceeds viewArea" class of fault and would confound the
result of this experiment.

This is ONLY half the switch. `native/carplay-jni/src/lib.rs` must also call
`levers::set_viewareas(true)`, or `"viewAreas"` is absent from the SETUP enabledFeatures echo and iOS
ignores the whole structure (`session.rs:664-666`).

    python3 tools/info_plist_viewareas.py            # apply
    python3 tools/info_plist_viewareas.py --revert   # back to one full-bleed area
    python3 tools/info_plist_viewareas.py --check
"""

import plistlib
import sys
from pathlib import Path

PLIST = Path(__file__).resolve().parent.parent / "netprobe_app/app/src/main/assets/info.bplist"

# The device-proven-good second area on a 2400x960 panel. See module docstring.
AREA2 = {"x": 188, "y": 118, "w": 1416, "h": 842}
INITIAL = 0


def panel(disp):
    return int(disp["widthPixels"]), int(disp["heightPixels"])


def validate(disp):
    pw, ph = panel(disp)
    x, y, w, h = AREA2["x"], AREA2["y"], AREA2["w"], AREA2["h"]
    errs = []
    if x + w > pw or y + h > ph or x < 0 or y < 0:
        errs.append(f"containment: {w}x{h}@{x},{y} not inside {pw}x{ph}")
    if any(v % 2 for v in (x, y, w, h)):
        errs.append("all four values must be even")
    if w <= 0 or h <= 0:
        errs.append("dimensions must be positive")
    if pw >= ph and (w < 800 or h < 480):
        errs.append(f"product floor 800x480 (landscape) violated by {w}x{h}")
    return errs


def area(x, y, w, h, main=True):
    d = {
        "originXPixels": x, "originYPixels": y, "widthPixels": w, "heightPixels": h,
        "viewAreaTransitionControl": True,   # both areas: an area you can leave but not return to is a trap
        "viewAreaStatusBarEdge": 0,          # Auto
        "viewAreaSupportsFocusTransfer": False,
        "safeArea": {"originXPixels": x, "originYPixels": y, "widthPixels": w, "heightPixels": h},
    }
    if main:
        d["safeArea"]["drawUIOutsideSafeArea"] = False
    return d


def main() -> int:
    d = plistlib.load(PLIST.open("rb"))
    disp = d["displays"][0]
    pw, ph = panel(disp)

    if "--check" in sys.argv:
        n = len(disp.get("viewAreas", []))
        print(f"viewAreas={n} initialViewArea={disp.get('initialViewArea')} "
              f"adjacentViewAreas={disp.get('adjacentViewAreas')}")
        return 0 if n == 2 else 1

    if "--revert" in sys.argv:
        disp["viewAreas"] = [area(0, 0, pw, ph)]
        disp["viewAreas"][0]["viewAreaTransitionControl"] = False
        disp["initialViewArea"] = 0
        disp["adjacentViewAreas"] = []
        plistlib.dump(d, PLIST.open("wb"), fmt=plistlib.FMT_BINARY, sort_keys=True)
        print("REVERTED to one full-bleed area, adjacentViewAreas=[]")
        return 0

    errs = validate(disp)
    if errs:
        for e in errs:
            print(f"FATAL: {e}", file=sys.stderr)
        return 2

    disp["viewAreas"] = [
        area(0, 0, pw, ph),
        area(AREA2["x"], AREA2["y"], AREA2["w"], AREA2["h"]),
    ]
    disp["initialViewArea"] = INITIAL
    # DERIVED, never constant — see the docstring's proven-bad case.
    disp["adjacentViewAreas"] = [1 - INITIAL]
    plistlib.dump(d, PLIST.open("wb"), fmt=plistlib.FMT_BINARY, sort_keys=True)

    back = plistlib.load(PLIST.open("rb"))["displays"][0]
    for i, a in enumerate(back["viewAreas"]):
        print(f"  viewAreas[{i}] {a['widthPixels']}x{a['heightPixels']}"
              f"@{a['originXPixels']},{a['originYPixels']} "
              f"transitionControl={a['viewAreaTransitionControl']}")
    print(f"  initialViewArea={back['initialViewArea']} adjacentViewAreas={back['adjacentViewAreas']}")
    print(f"OK — panel {pw}x{ph}, {PLIST.name}: {PLIST.stat().st_size} bytes")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
