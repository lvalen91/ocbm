#!/usr/bin/env python3
"""Declare `limitedUIElements` in the served /info — the bitmask `setLimitedUI` actually restricts.

WHY
---
`setLimitedUI` was reaching iOS and doing NOTHING. Owner-observed 2026-09-08: with the macOS host's
Controls "Drive" toggle the Apple Maps keyboard icon disappears leaving only Siri; with this app, on
the same phone, nothing changed — while the app logged `setLimitedUI(true) sent`, i.e. the encrypted
frame went out.

The command was never the problem. **`limitedUI` is a boolean; `limitedUIElements` is the SET it
applies to.** iOS 27 CarKit carries both on `CARScreenInfo` — `B _limitedUI` and `Q
_limitedUIElements` (a bitmask) — and `CARSessionConfiguration._limitableUserInterfaces` is built by
`+_limitableUserInterfacesFromLimitedUIValues:` from the `/info` STRING ARRAY. So with no array the
mask is 0, and `setLimitedUI(true)` faithfully restricts the empty set. No error, no log, no effect.

This also retires a stale "REFUTED" result. `docs/carplay/03_SDK_GROUND_TRUTH.md` and the comment at
`crates/vendor/receiver/src/info.rs` recorded `setLimitedUI` as verified-sent-and-ignored on hardware
(2026-07-30) and explicitly ruled OUT the element list as the gate, reasoning that Apple's own
Simulator ships an empty list yet its toggle works. That test ran on a build whose YAML carried no
`limitedUIConfig` at all — the feature landed the same day — so the mask was empty in the failing case
too. The "Apple's own is empty in a working session" half was never actually observed.

WHAT THE BOX SERVES, AND WHY WE MIRROR IT
-----------------------------------------
The macOS path builds /info through `receiver::info` from app-pushed config: the Settings window
emits a `limitedUIConfig:` YAML block, `OCBMClient` pushes it on SUBSCRIBE, `ocbmd` lands it at
`/tmp/carplay_cfg.yaml`, `airplayd` parses it per control connection, and `vehicle_config.rs` turns it
into `limited_ui_elements`. With the owner's current settings that resolves to:

    ["softKeyboard", "softPhoneKeypad", "musicLists", "longUserAlert"]

We declare exactly that set, because it is the one configuration observed WORKING on this phone. It is
a mirror of a proven state, not a guess at a better one — `nonMusicLists` and `japanMaps` are
deliberately left out for that reason alone.

`softKeyboard` is the element behind the Apple Maps keyboard icon, which is the specific thing the
owner watched change. Restricting it is the whole point of the exercise.

NAMING TRAPS (each was got wrong by inference first, upstream)
  * Apple's emission order is `softKeyboard, softPhoneKeypad, musicLists, nonMusicLists, japanMaps,
    longUserAlert` — `musicLists` BEFORE `nonMusicLists`, the reverse of R14G17's header order.
  * The config key `longAlerts` goes on the wire as **`longUserAlert`**. It is the only element whose
    config name and wire string differ.
  * Four further real `LimitedUIConfig` keys (`pairedDevices`, `themeCustomization`,
    `automakerSettings`, `automakerSettingsInfoButton`) are NOT emitted by Apple's own
    `airPlayElements` getter. iOS 27 CarKit's parser does accept them, but emitting what the reference
    emitter does not is a deviation, so we do not.

`limitedUI` (the initial-state bool) is ALREADY in this plist as `false` and must stay: CarPlay starts
unrestricted and the runtime command moves it. Whether it is independently required is untested —
both the working and non-working paths carry it, so it is not the differentiator.

    python3 tools/info_plist_limitedui.py            # apply
    python3 tools/info_plist_limitedui.py --revert   # remove the array again
    python3 tools/info_plist_limitedui.py --check
"""

import plistlib
import sys
from pathlib import Path

PLIST = Path(__file__).resolve().parent.parent / "netprobe_app/app/src/main/assets/info.bplist"

# Apple's full emission order. Index here is meaningful only as documentation of that order.
APPLE_ORDER = [
    "softKeyboard",
    "softPhoneKeypad",
    "musicLists",
    "nonMusicLists",
    "japanMaps",
    "longUserAlert",
]

# The set the macOS box is serving in the session where the toggle demonstrably works.
ELEMENTS = ["softKeyboard", "softPhoneKeypad", "musicLists", "longUserAlert"]


def main() -> int:
    d = plistlib.load(PLIST.open("rb"))

    if "--check" in sys.argv:
        els = d.get("limitedUIElements")
        print(f"limitedUI={d.get('limitedUI')} limitedUIElements={els}")
        return 0 if els == ELEMENTS else 1

    if "--revert" in sys.argv:
        d.pop("limitedUIElements", None)
        plistlib.dump(d, PLIST.open("wb"), fmt=plistlib.FMT_BINARY, sort_keys=True)
        print(f"REVERTED — limitedUIElements removed, {PLIST.name}: {PLIST.stat().st_size} bytes")
        return 0

    unknown = [e for e in ELEMENTS if e not in APPLE_ORDER]
    if unknown:
        print(f"FATAL: not an Apple element name: {unknown}", file=sys.stderr)
        return 2

    # Emit in Apple's order regardless of how ELEMENTS happens to be written above.
    d["limitedUIElements"] = [e for e in APPLE_ORDER if e in ELEMENTS]
    # Initial state stays unrestricted; the runtime /command moves it. Assert rather than set, so a
    # deliberate change elsewhere is not silently reverted by running this tool.
    if d.get("limitedUI") is not False:
        print(f"WARNING: limitedUI is {d.get('limitedUI')!r}, expected False", file=sys.stderr)
    plistlib.dump(d, PLIST.open("wb"), fmt=plistlib.FMT_BINARY, sort_keys=True)

    back = plistlib.load(PLIST.open("rb"))
    print(f"  limitedUI          = {back.get('limitedUI')}")
    print(f"  limitedUIElements  = {back.get('limitedUIElements')}")
    print(f"OK — {PLIST.name}: {PLIST.stat().st_size} bytes")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
