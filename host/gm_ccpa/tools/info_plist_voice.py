#!/usr/bin/env python3
"""Advertise AAC-ELD 48 kHz mono alongside 16 kHz on the voice audio entries of /info.

WHY
---
`/info` here is a STATIC binary plist served verbatim by the Rust core (`CarPlayRx.kt` reads
`assets/info.bplist`); the upstream generator `receiver::info::preset_wireless_8()` is never run by
this app, so this file is the only place the advertised audio formats can change.

The three type-100 voice entries (`default`, `telephony`, `speechRecognition`) advertised
`audioOutputFormats = 0x04000000` — bit 26, AAC-ELD **16 kHz** mono. Bit 32 is AAC-ELD **48 kHz**
mono (`session.rs:2704`, `1u64 << 32`), the highest-quality voice format iOS will negotiate. Opus
(bits 28-30) was rejected as the alternative: the box never decodes it by design
(`session.rs:2679-2681`), so it would be a pure host-side gamble on an untested OMX decoder.

We advertise BOTH bits, not just bit 32. `audioFormats` is a capability mask and iOS chooses: if it
declines 48 kHz it simply picks 16 kHz and nothing is lost. Advertising bit 32 alone would mean a
decline shows up as the SETUP for that audioType never arriving at all — no stream, no error, silent
loss of Siri and call audio. That is the "never advertise what you cannot fall back from" shape of
`docs/05` ordering rule 10.

`audioInputFormats` is deliberately left at bit 26. The mic encoder is 16 kHz-only
(`MicUplink.kt`), and Apple's bitrate ladder would expect 96 kbps above 32 kHz while the shim pins
48 kbps — raising the input without touching `eld_shim.c` would under-code.

Which rate iOS actually picked is visible in the log: `[voice] <purpose>: AAC-ELD <rate>Hz <ch>ch`.

    python3 tools/info_plist_voice.py            # apply
    python3 tools/info_plist_voice.py --check    # exit 1 if not applied
"""

import plistlib
import sys
from pathlib import Path

PLIST = Path(__file__).resolve().parent.parent / "netprobe_app/app/src/main/assets/info.bplist"

ELD_16K_MONO = 1 << 26          # 0x04000000
ELD_48K_MONO = 1 << 32          # 0x100000000
WANT_OUT = ELD_16K_MONO | ELD_48K_MONO   # 4362076160

VOICE_TYPES = {"default", "telephony", "speechRecognition"}


def voice_entries(d):
    """The type-100 entries that carry speech. type 101/102 and `alert`/`compatibility` are other lanes."""
    for e in d.get("audioFormats", []):
        if e.get("type") == 100 and e.get("audioType") in VOICE_TYPES and "audioOutputFormats" in e:
            yield e


def main() -> int:
    if not PLIST.exists():
        print(f"FATAL: {PLIST} not found", file=sys.stderr)
        return 2
    d = plistlib.load(PLIST.open("rb"))
    entries = list(voice_entries(d))
    if not entries:
        print("FATAL: no type-100 voice entries found in audioFormats", file=sys.stderr)
        return 2

    if "--check" in sys.argv:
        ok = all(e["audioOutputFormats"] == WANT_OUT for e in entries)
        for e in entries:
            print(f"  {e['audioType']:<18} out=0x{e['audioOutputFormats']:x} "
                  f"in=0x{e.get('audioInputFormats', 0):x}")
        print("OK" if ok else "MISSING")
        return 0 if ok else 1

    for e in entries:
        was = e["audioOutputFormats"]
        e["audioOutputFormats"] = WANT_OUT
        print(f"  {e['audioType']:<18} audioOutputFormats 0x{was:x} -> 0x{WANT_OUT:x} "
              f"(ELD 16k mono | ELD 48k mono); audioInputFormats left at "
              f"0x{e.get('audioInputFormats', 0):x}")
    with PLIST.open("wb") as f:
        plistlib.dump(d, f, fmt=plistlib.FMT_BINARY, sort_keys=True)

    back = plistlib.load(PLIST.open("rb"))
    if not all(e["audioOutputFormats"] == WANT_OUT for e in voice_entries(back)):
        print("FATAL: wrote the plist but the change did not take", file=sys.stderr)
        return 2
    print(f"OK — {PLIST.name}: {PLIST.stat().st_size} bytes")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
