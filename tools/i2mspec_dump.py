#!/usr/bin/env python3
"""Decode Apple's iAP2 message spec archives into readable JSON.

Source: `iap2messages-internal.i2mspecarchive` in Xcode's
CarPlaySimulator.devicekitplugin (iAP2MessageKit.framework/Resources). The file is an
NSKeyedArchiver graph of `I2MMessageDefinition` / `I2MParameterDefinition` /
`I2MGroupParameterDefinition` / `I2MEnumParameterDefinition` objects. This walks it and
emits, per message: id, name, source, and the full parameter tree (id, name, type,
cardinality, note, nested parameters, enum values).

It is the same archive `iap2-core/src/spec.rs` was generated from, and it is the
authority for TLV parameter ids — do not hand-derive those.

Usage:
    tools/i2mspec_dump.py > spec.json               # all 144 messages
    tools/i2mspec_dump.py --message 0x4158          # one message
    tools/i2mspec_dump.py --message 0x4158 --text   # readable parameter table
    tools/i2mspec_dump.py --archive <path> ...
"""

import json
import plistlib
import sys

DEFAULT = ("/Applications/Xcode.app/Contents/SharedFrameworks/DeviceKit.framework/Versions/A/"
           "PlugIns/CarPlaySimulator.devicekitplugin/Contents/Frameworks/iAP2MessageKit.framework/"
           "Versions/A/Resources/iap2messages-internal.i2mspecarchive")


class Archive:
    def __init__(self, path):
        self.objs = plistlib.load(open(path, "rb"))["$objects"]

    def deref(self, x):
        return self.objs[x.data] if isinstance(x, plistlib.UID) else x

    def str_(self, x):
        v = self.deref(x)
        return v if isinstance(v, str) else None

    def list_(self, x):
        v = self.deref(x)
        if isinstance(v, dict) and "NS.objects" in v:
            return [self.deref(o) for o in v["NS.objects"]]
        return []

    def type_(self, x):
        v = self.deref(x)
        if not isinstance(v, dict):
            return None
        return self.str_(v.get("typeString")) or v.get("typeInEnum")

    def int_(self, x):
        v = self.deref(x)
        return v if isinstance(v, int) else None

    def param(self, p):
        out = {
            "id": self.int_(p.get("identifier")),
            "name": self.str_(p.get("name")),
            "type": self.type_(p.get("type")),
            "count": self.int_(p.get("countExpressionEnum")),
        }
        note = self.str_(p.get("note"))
        if note:
            out["note"] = note
        if "parameterDefinitions" in p:
            out["parameters"] = [self.param(c) for c in self.list_(p["parameterDefinitions"])]
        if "enumDefinitions" in p:
            out["enumName"] = self.str_(p.get("enumName"))
            out["enum"] = {
                self.int_(e.get("value")): self.str_(e.get("valueDescription"))
                for e in self.list_(p["enumDefinitions"]) if isinstance(e, dict)
            }
        return out

    def messages(self):
        for o in self.objs:
            if not isinstance(o, dict) or "parameterDefinitions" not in o:
                continue
            if "source" not in o or "identifier" not in o:
                continue
            yield {
                "id": self.int_(o["identifier"]),
                "name": self.str_(o.get("name")),
                "source": self.int_(o.get("source")),
                "parameters": [self.param(p) for p in self.list_(o["parameterDefinitions"])],
            }


def text(m):
    src = {0: "accessory", 1: "device"}.get(m["source"], str(m["source"]))
    print(f"0x{m['id']:04X}  {m['name']}   (source: {src})")
    if not m["parameters"]:
        print("  (no parameters)")

    def show(ps, ind="  "):
        for p in ps:
            # countExpressionEnum. CORRECTED 2026-07-30 — the previous legend
            # ({0:"", 1:"[optional]", 2:"[repeated]"}) had 1 INVERTED: it printed "[optional]"
            # for every REQUIRED parameter, and omitted the genuinely-optional case 3 entirely.
            # Decoded from `+[I2MTypeUtils ...]` in iAP2MessageKitCore (`sub w8,w0,#1; cmp w8,#2;
            # b.hs` -> values 1 and 2 branch to the REQUIRED bucket, 3 to the optional bucket, 0 to
            # neither), and corroborated by the CarPlay Simulator's own Identify encoder, which
            # emits param-30 subs 0/1 unconditionally and nil-checks subs 2..8.
            #   0 = zero or more   (optional, repeatable)
            #   1 = exactly one    (REQUIRED)
            #   2 = one or more    (REQUIRED, repeatable)
            #   3 = zero or one    (optional)
            # This legend has now been wrong twice in this project's history; both times it produced
            # a confident false reading of Apple's spec. Do not "simplify" it without re-deriving.
            cnt = {
                0: " [0+]",
                1: " [required]",
                2: " [1+ required]",
                3: " [optional]",
            }.get(p.get("count"), f" [count={p.get('count')}]")
            print(f"{ind}{p['id']:>3}  {p['name']}  <{p['type']}>{cnt}")
            if p.get("note"):
                print(f"{ind}     note: {p['note']}")
            if p.get("enum"):
                for v, d in sorted(p["enum"].items(), key=lambda kv: (kv[0] is None, kv[0])):
                    print(f"{ind}     {v} = {d}")
            if p.get("parameters"):
                show(p["parameters"], ind + "    ")

    show(m["parameters"])


def main():
    args = sys.argv[1:]
    path, want, as_text = DEFAULT, None, False
    while args:
        a = args.pop(0)
        if a == "--message":
            want = int(args.pop(0), 0)
        elif a == "--archive":
            path = args.pop(0)
        elif a == "--text":
            as_text = True
        else:
            path = a

    msgs = list(Archive(path).messages())
    if want is not None:
        msgs = [m for m in msgs if m["id"] == want]
        if not msgs:
            print(f"no message 0x{want:04X} in {path}", file=sys.stderr)
            sys.exit(1)
    if as_text:
        for m in msgs:
            text(m)
            print()
    else:
        json.dump(msgs, sys.stdout, indent=1, default=str)
        print()


if __name__ == "__main__":
    main()
