#!/usr/bin/env python3
"""Fail the docs build if a <seyd-connect-error> failure class has no page.

The element links every diagnosis to `https://docs.seyd.io/networking/<class>`
(sdks/js/web/src/seyd-connect-error.ts). This check reads the `FailureClass`
union from that file and requires a page per member under
web/docs/src/content/docs/networking/classes/, so the SDK's links can never
dangle (PLAN.md §2.8). Run from anywhere; paths are resolved from the repo.
"""
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / "sdks/js/web/src/seyd-connect-error.ts"
PAGES = ROOT / "web/docs/src/content/docs/networking/classes"


def failure_classes(text: str) -> list[str]:
    m = re.search(r"export type FailureClass\s*=\s*(.*?);", text, re.S)
    if not m:
        sys.exit(f"{SOURCE}: FailureClass union not found")
    return re.findall(r"'([a-z0-9-]+)'", m.group(1))


def main() -> int:
    classes = failure_classes(SOURCE.read_text())
    missing = [c for c in classes if not any((PAGES / f"{c}{ext}").exists() for ext in (".md", ".mdx"))]
    extra = [p.stem for p in PAGES.glob("*.md*") if p.stem not in classes and p.stem != "index"]
    if missing:
        print(f"networking: no page for failure class(es) {missing} under {PAGES.relative_to(ROOT)}", file=sys.stderr)
    if extra:
        print(f"networking: page(s) {extra} match no FailureClass in {SOURCE.relative_to(ROOT)}", file=sys.stderr)
    if missing or extra:
        return 1
    print(f"networking: {len(classes)} failure classes, all documented")
    return 0


if __name__ == "__main__":
    sys.exit(main())
