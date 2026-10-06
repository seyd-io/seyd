#!/usr/bin/env python3
"""Fail when a tracked source file lacks the license header (docs/open-source.md).

The public trees must carry the Apache-2.0 header and the private trees the
proprietary one; `tools/add-headers.py` adds either. Run from anywhere; CI
runs it on every pull request.
"""
import importlib.util
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("add_headers", HERE / "add-headers.py")
ah = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ah)


def main() -> int:
    missing = []
    for private in (False, True):
        for path in ah.tracked_files():
            if path.is_file() and ah.wants_header(path, private) and not ah.has_header(path.read_text()):
                missing.append(path.relative_to(ah.ROOT).as_posix())
    if missing:
        print("check-headers: no license header in:", file=sys.stderr)
        for m in missing:
            print(f"  {m}", file=sys.stderr)
        print("run tools/add-headers.py (and --private for the private trees)", file=sys.stderr)
        return 1
    print("check-headers: every source file carries its header")
    return 0


if __name__ == "__main__":
    sys.exit(main())
