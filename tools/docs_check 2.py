#!/usr/bin/env python3
"""Enforce the docs/ invariants set on 2026-08-31.

The corpus was 66 flat files. Corrections landed as NEW documents instead of edits to the owning
one, so every topic had several files of different vintage and a reader -- human or model -- that
skimmed the wrong one acted on a refuted claim. The fix is structural, and these are the rules that
hold it:

  * <= MAX_PER_CATEGORY .md files per category directory. When a topic needs a new file, something
    else merges first. This is the rule that stops the regrowth.
  * docs/ root holds README.md and nothing else -- no file escapes a category.
  * every document opens with a STATUS line, so its standing is visible without reading it.
  * every relative markdown link resolves. Consolidation moved every path once; a dangling link is
    how a reader ends up back at a file that no longer exists.

Exit 1 on any violation. Run it before committing a docs change.
"""

import re
import sys
from pathlib import Path

DOCS = Path(__file__).resolve().parent.parent / "docs"
MAX_PER_CATEGORY = 10
CATEGORIES = ("carplay", "androidauto", "wireless", "host", "ops")
STATUS_RE = re.compile(r"^> \*\*STATUS:\*\* (CURRENT|HISTORICAL RECORD|SUPERSEDED-BY-\S+)\b", re.M)
LINK_RE = re.compile(r"\[[^\]]*\]\(([^)#]+)(?:#[^)]*)?\)")

def main() -> int:
    errors: list[str] = []

    stray = sorted(p.name for p in DOCS.glob("*.md") if p.name != "README.md")
    if stray:
        errors.append(f"docs/ root must hold only README.md; found: {', '.join(stray)}")

    for cat in CATEGORIES:
        d = DOCS / cat
        if not d.is_dir():
            errors.append(f"missing category directory docs/{cat}/")
            continue
        docs = sorted(d.glob("*.md"))
        if len(docs) > MAX_PER_CATEGORY:
            errors.append(
                f"docs/{cat}/ has {len(docs)} documents, cap is {MAX_PER_CATEGORY} -- "
                f"merge before adding: {', '.join(p.name for p in docs)}"
            )
        for p in docs:
            if not STATUS_RE.search(p.read_text()):
                errors.append(f"{p.relative_to(DOCS.parent)}: no STATUS line")

    for p in sorted(DOCS.rglob("*.md")):
        for link in LINK_RE.findall(p.read_text()):
            if link.startswith(("http://", "https://", "mailto:")):
                continue
            target = (p.parent / link).resolve()
            if not target.exists():
                errors.append(f"{p.relative_to(DOCS.parent)}: dead link -> {link}")

    counts = {c: len(list((DOCS / c).glob('*.md'))) for c in CATEGORIES if (DOCS / c).is_dir()}
    print("docs: " + " · ".join(f"{c} {n}/{MAX_PER_CATEGORY}" for c, n in counts.items()))
    if errors:
        print(f"\n{len(errors)} problem(s):", file=sys.stderr)
        for e in errors[:60]:
            print(f"  {e}", file=sys.stderr)
        if len(errors) > 60:
            print(f"  ... and {len(errors)-60} more", file=sys.stderr)
        return 1
    print("OK")
    return 0

if __name__ == "__main__":
    sys.exit(main())
