#!/usr/bin/env python3
"""pkill_selfmatch_check.py FILE...: refuse a shell script whose detached `sh -c '...'` blocks would kill
themselves. Such a block's text IS that shell's argv, so `pkill -f "[x]yz"` (the bracket form that keeps
the pattern from matching its own command line) is defeated by ANY plain "xyz" elsewhere in the block,
comments included. Found on the baseline 2026-10-30: session_supervisor.sh's radio teardown carried
"# btmon ..." beside `pkill -f "[b]tmon"`, killed itself after its first pkill, and never took the radios
down. Exit 1 with the offending lines; 0 when clean. Run by tools/ocbm_overlay.sh before packing."""
import re
import sys

# `pkill [-9] -f "[x]yz"`, and since 2026-10-30 also `grep -<flags> "[x]yz"`: the /proc/*/cmdline scan that
# replaced pgrep -f for btd (BusyBox pgrep -f sees only the first ~1 KiB of a command line) has the same rule.
PAT = re.compile(r'(?:pkill(?:\s+-9)?\s+-f|grep\s+-[A-Za-z]+)\s+"\[(.)\]([^"]*)"')


def blocks(text):
    lines = text.split("\n")
    i = 0
    while i < len(lines):
        if re.search(r"\bsh -c '\s*$", lines[i]) or re.search(r"\bsh -c '[^']*$", lines[i]):
            start = i
            j = i + 1
            while j < len(lines) and not re.match(r"\s*'(\s|$)", lines[j]):
                j += 1
            yield start + 1, "\n".join(lines[start:j + 1])
            i = j + 1
        else:
            i += 1


def main():
    bad = 0
    for path in sys.argv[1:]:
        text = open(path, encoding="utf-8", errors="replace").read()
        for lineno, block in blocks(text):
            for m in PAT.finditer(block):
                word = m.group(1) + m.group(2)
                if word in block:
                    bad += 1
                    print("%s:%d: the sh -c block starting here contains the plain text %r, which its own "
                          "`pkill -f`/`grep` \"[%s]%s\" matches (self-kill)" % (path, lineno, word, m.group(1), m.group(2)))
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
