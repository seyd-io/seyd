#!/usr/bin/env python3
"""Add the license header to every source file the project wrote (docs/open-source.md).

Two lines in the file's own comment syntax, after a shebang if there is one:

    // Copyright 2026 Anton Gravestam
    // SPDX-License-Identifier: Apache-2.0

Idempotent: a file whose first lines already carry an SPDX identifier is left
alone. Generated files (seyd.h: cbindgen carries the header from its config),
vendored code, lockfiles, build output and anything under the private trees
(cloud/, deploy/, web/console/, business/) are skipped; `--private` headers
those trees instead with the all-rights-reserved form. The year is the year
of first publication and is not updated annually.

    tools/add-headers.py            # add where missing, print each file touched
    tools/add-headers.py --private  # the private trees, with the proprietary header
    tools/check-headers.py          # CI: fail on a source file without a header

In the private seyd-cloud repository, where this script is reached through
the `seyd/` submodule and every file is proprietary, pass `--root .`
together with `--private`; the submodule itself is a gitlink, not a file,
so it is never touched from there.
"""
import argparse
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]  # overridden by --root

HOLDER = "Copyright 2026 Anton Gravestam"
PUBLIC = [HOLDER, "SPDX-License-Identifier: Apache-2.0"]
PRIVATE = [HOLDER + ". All rights reserved.", "SPDX-License-Identifier: LicenseRef-Seyd-Proprietary"]

# Extension → (line prefix, block open, block close). Block style only where line comments do not exist.
STYLES = {
    ".rs": ("// ", None, None), ".ts": ("// ", None, None), ".mts": ("// ", None, None), ".js": ("// ", None, None),
    ".mjs": ("// ", None, None), ".c": ("// ", None, None), ".h": ("// ", None, None), ".cpp": ("// ", None, None),
    ".hpp": ("// ", None, None),
    ".py": ("# ", None, None), ".sh": ("# ", None, None), ".toml": ("# ", None, None), ".yml": ("# ", None, None),
    ".yaml": ("# ", None, None), ".Makefile": ("# ", None, None),
    ".css": (" * ", "/*", " */"),
    ".html": ("  ", "<!--", "-->"),  # a comment may precede <!doctype>
    ".astro": ("", "---", "---"),  # handled specially: inside the front-matter fence
}

PRIVATE_TREES = ("cloud/", "deploy/", "web/console/", "business/")
SKIP_PREFIXES = ("node_modules/", "target/", "dist/", ".astro/", "tools/.venv/")
SKIP_FILES = {
    "sdks/c/include/seyd.h",       # cbindgen output; the header is in packages/seyd-ffi/cbindgen.toml
    "pnpm-lock.yaml", "Cargo.lock",
}
SKIP_SUFFIXES = (".lock", ".min.js", ".d.ts")
SKIP_NAMES = {"package.json"}


def tracked_files() -> list[Path]:
    out = subprocess.run(["git", "ls-files", "-z"], cwd=ROOT, check=True, capture_output=True).stdout
    return [ROOT / p for p in out.decode().split("\0") if p]


def style_of(path: Path):
    if path.name == "Makefile":
        return STYLES[".Makefile"]
    return STYLES.get(path.suffix)


def wants_header(path: Path, private: bool) -> bool:
    rel = path.relative_to(ROOT).as_posix()
    if rel.startswith(SKIP_PREFIXES) or "/node_modules/" in rel or "/dist/" in rel or "/target/" in rel:
        return False
    if rel in SKIP_FILES or rel.endswith(SKIP_SUFFIXES) or path.name in SKIP_NAMES:
        return False
    if style_of(path) is None:
        return False
    # A repository that is private as a whole (seyd-cloud) has no public tree.
    in_private = rel.startswith(PRIVATE_TREES) or not (ROOT / "LICENSE").exists()
    return in_private if private else not in_private


def has_header(text: str) -> bool:
    return "SPDX-License-Identifier:" in "\n".join(text.splitlines()[:8])


def with_header(path: Path, text: str, lines: list[str]) -> str:
    prefix, open_, close = style_of(path)
    if path.suffix == ".astro":
        # Astro front matter: `---\n…\n---`. Put the header as the first lines of the fence, creating one if absent.
        if text.startswith("---\n"):
            return "---\n" + "".join(f"// {l}\n" for l in lines) + text[4:]
        return "---\n" + "".join(f"// {l}\n" for l in lines) + "---\n" + text
    if open_:
        block = f"{open_}\n" + "".join(f"{prefix}{l}\n" for l in lines) + f"{close}\n"
    else:
        block = "".join(f"{prefix}{l}\n" for l in lines)
    if text.startswith("#!"):
        nl = text.index("\n") + 1
        return text[:nl] + block + text[nl:]
    return block + text


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--private", action="store_true", help="header the private trees with the proprietary form")
    ap.add_argument("--root", type=Path, help="the repository to work in (default: the one this script is in)")
    args = ap.parse_args()
    if args.root:
        global ROOT
        ROOT = args.root.resolve()
    lines = PRIVATE if args.private else PUBLIC
    touched = 0
    for path in tracked_files():
        if not path.is_file() or not wants_header(path, args.private):
            continue
        text = path.read_text()
        if has_header(text):
            continue
        path.write_text(with_header(path, text, lines))
        print(path.relative_to(ROOT))
        touched += 1
    print(f"add-headers: {touched} file(s) headed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
