#!/bin/sh
# extract_iap2_trace.sh FILE [FILE...]
# Pull the accessoryd `LOG; ...` iAP2 packet trace out of a capture (live stream
# or `log show` dump) and render it as a readable table.
#
# Guards against the audioaccessoryd (AirPods) false match by requiring the line
# to come from the `accessoryd[` process AND contain the `LOG;` sentinel.
#
# Line grammar (fields separated by "; "):
#   LOG; <reltime>; <endpointUUIDpair>; <transport>; <Acc|iPod|Event>; ...
#   Acc/iPod : len=0x..; control=0x..; seq=0x..; ack=0x..; session=0x..; hdrChk=0x..; payload(len=N chk=0x..)=<hex>
#   Event    : <NAME>; <human text>
python3 - "$@" <<'PY'
import sys, re

CTRL = {  # iAP2 link control byte -> name (from docs/carplay/05_METADATA_AND_CONTROLS.md + iAP2Link.c)
    0xee: "DETECT", 0x80: "SYN", 0xc0: "SYN-ACK", 0x40: "ACK",
    0x00: "DATA", 0xff: "RST",
}
def ctrl_name(v):
    try: n = int(v, 16)
    except Exception: return ""
    if n in CTRL: return CTRL[n]
    bits = []
    if n & 0x80: bits.append("SYN")
    if n & 0x40: bits.append("ACK")
    if n & 0x20: bits.append("EAK")
    if n & 0x10: bits.append("RST")
    if n & 0x08: bits.append("SLP")
    return "|".join(bits)

rows = []
files = sys.argv[1:]
if not files:
    sys.exit("usage: extract_iap2_trace.sh FILE [FILE...]  (reads capture files, not stdin)")
for fn in files:
    f = open(fn, errors="replace")
    for line in f:
        # require ` accessoryd[` (leading space) so audioaccessoryd (AirPods) never matches
        if "LOG;" not in line or " accessoryd[" not in line:
            continue
        i = line.find("LOG;")
        parts = [p.strip() for p in line[i:].rstrip().split(";")]
        # parts[0]="LOG", [1]=reltime, [2]=endpoint, [3]=transport, [4]=kind, ...
        if len(parts) < 5: continue
        t, transport, kind = parts[1], parts[3], parts[4]
        rec = {"t": t, "xport": transport, "kind": kind,
               "ctrl": "", "name": "", "seq": "", "ack": "", "sess": "",
               "len": "", "payload": ""}
        if kind in ("Acc", "iPod"):
            blob = ";".join(parts[5:])
            def g(k, s=blob):
                m = re.search(k + r"=\s*(0x[0-9a-fA-F]+|\d+)", s); return m.group(1) if m else ""
            rec["len"]  = g("len")
            rec["ctrl"] = g("control")
            rec["name"] = ctrl_name(rec["ctrl"])
            rec["seq"], rec["ack"], rec["sess"] = g("seq"), g("ack"), g("session")
            pm = re.search(r"payload\(len=(\d+).*?\)=<([0-9A-Fa-f ]*)>", blob)
            if pm:
                rec["payload"] = pm.group(2).strip()
        else:  # Event
            rec["name"] = parts[5] if len(parts) > 5 else ""
            rec["payload"] = parts[6] if len(parts) > 6 else ""
        rows.append(rec)
    f.close()

if not rows:
    print("(no `LOG;` iAP2 packet-trace lines found -- was PrintIapPackets set and "
          "accessoryd restarted before the session? see capture script header.)")
    sys.exit(0)

hdr = ("RELTIME", "TRANSPORT", "DIR", "CTRL", "NAME", "SEQ", "ACK", "SES", "LEN", "PAYLOAD / TEXT")
w =   (10,         18,          5,     6,      9,      6,     6,     5,     8,     0)
def fmt(cols):
    out = []
    for c, width in zip(cols, w):
        c = str(c)
        out.append(c.ljust(width) if width else c)
    return "  ".join(out).rstrip()
print(fmt(hdr))
print(fmt(["-"*min(len(h),wd or len(h)) for h, wd in zip(hdr, w)]))
for r in rows:
    print(fmt([r["t"], r["xport"], r["kind"], r["ctrl"], r["name"],
               r["seq"], r["ack"], r["sess"], r["len"], r["payload"]]))

# quick summary: per-transport, per-control counts (catches the "31x same SYN-ACK" pattern)
from collections import Counter
print("\n-- summary (transport / dir / control) --")
c = Counter((r["xport"], r["kind"], r["name"] or r["ctrl"]) for r in rows)
for (xp, k, nm), n in sorted(c.items(), key=lambda x:(-x[1])):
    print(f"  {n:4d}  {xp:18s} {k:5s} {nm}")
PY
