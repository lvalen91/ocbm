#!/usr/bin/env python3
"""D-F1 lint (plan 2.4): no two `PendingIntent.getActivity(` call sites that target MainActivity may
share a request-code argument. getActivity keys on (requestCode, Intent.filterEquals) and extras are
NOT part of the key, so two sites with one code and one component hand each other's PendingIntent
back (the media card got the notification's untagged intent, 2026-10-08). Run by tools/test.sh.

Usage: pending_intent_lint.py <main-java-root>   exit 0 clean, 1 duplicate, 2 nothing found.
"""
import os, re, sys

def calls(text):
    """Yield (call_text, line_no) for every PendingIntent.getActivity( ... ) with balanced parens."""
    for m in re.finditer(r"PendingIntent\.getActivity\(", text):
        i, depth = m.end(), 1
        while i < len(text) and depth:
            depth += {"(": 1, ")": -1}.get(text[i], 0)
            i += 1
        yield text[m.start():i], text.count("\n", 0, m.start()) + 1

def second_arg(call):
    body = call[call.index("(") + 1:-1]
    args, depth, cur = [], 0, ""
    for ch in body:
        if ch in "([{": depth += 1
        elif ch in ")]}": depth -= 1
        if ch == "," and depth == 0:
            args.append(cur); cur = ""
        else:
            cur += ch
    args.append(cur)
    return args[1].strip() if len(args) > 1 else ""

root = sys.argv[1]
sites = []
for dp, _, fns in os.walk(root):
    for fn in fns:
        if not fn.endswith(".kt"):
            continue
        p = os.path.join(dp, fn)
        with open(p, encoding="utf-8") as f:
            text = f.read()
        for call, ln in calls(text):
            if "MainActivity" in call:
                sites.append((second_arg(call), os.path.relpath(p, root), ln))
if not sites:
    print("[Tier-0] FAIL: pending_intent_lint found no PendingIntent.getActivity(...MainActivity...) site at all")
    sys.exit(2)
by_code = {}
for code, p, ln in sites:
    by_code.setdefault(code, []).append(f"{p}:{ln}")
dups = {c: v for c, v in by_code.items() if len(v) > 1}
for code, p, ln in sites:
    print(f"    getActivity requestCode={code!r} at {p}:{ln}")
if dups:
    for c, v in dups.items():
        print(f"[Tier-0] FAIL: request code {c!r} is shared by {', '.join(v)} (PendingIntents collide)")
    sys.exit(1)
print(f"[Tier-0] pending-intent lint: {len(sites)} MainActivity site(s), all distinct request codes")
