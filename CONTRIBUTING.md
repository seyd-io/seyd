# Contributing to Seyd

Thank you. This file is short because the rules live where they are
enforced: `CLAUDE.md` is the engineering guide (written for a coding agent,
binding for people too), the docs build checks the documentation, and CI runs
the verification set on every pull request.

## Before you start

- Open an issue first for anything beyond a small fix, so the design is
  agreed before the code. Changes to a wire format, a public API or a product
  decision need an architecture decision record in `docs/adr/` (`CLAUDE.md`,
  "Key documents"); propose it in the issue.
- The fixed product decisions in `CLAUDE.md` (direct first, the relay last
  and always shown; no transcoding; Rust core with thin wrappers) are not
  reopened in a pull request.

## The verification set

Run it before opening a pull request; CI runs the same:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings
python3 tools/fec-vectors.py | cargo run -p seyd-fec --example check
make -C sdks/c check
tools/.venv/bin/python3 -m pytest sdks/python/tests
pnpm -r build && pnpm -r test
python3 tools/check-headers.py
cargo deny check licenses
```

`tools/setup-machine.sh` installs the toolchain on a fresh Mac;
`SEYD_DOCS_SKIP_RUSTDOC=1` skips the slow `cargo doc` step of the docs build
locally.

## Documentation is part of the change

A public surface (the C header, the Python package, the `@seyd/core` and
`@seyd/web` exports, `seydd.toml`, the QoS constants, the failure classes)
is not changed until the docs build passes and the guide that explains it
says the new thing, in the same pull request. The references are generated
from doc comments, so write the comment where the code is; examples are real
files under an SDK's `examples/`, never code blocks in a page; and the
integration skill in `skills/seyd/` must mention every public surface
(`tools/docs/gen-skill.py --check` fails the build otherwise). Every measured
number quoted in a document is re-measured when the code it describes
changes.

## Developer Certificate of Origin

Contributions are accepted under the
[Developer Certificate of Origin 1.1](https://developercertificate.org/):
by signing off a commit you certify that you wrote the change or have the
right to submit it under the project's license (Apache-2.0). Sign off with
`git commit -s`, which adds

```
Signed-off-by: Your Name <you@example.com>
```

Every commit in a pull request must carry it; CI checks.

## Commit messages

A short imperative subject naming the component (`seydd: …`, `@seyd/web: …`,
`Docs: …`), then a body that says what changed and why, including the
measurement when the change is about a number. Read `git log` for the voice.

## Code

Match the surrounding code: comment density, naming, idiom. Rust is
`cargo fmt` and clippy-clean with `-D warnings`; TypeScript is strict. New
source files carry the two-line license header (`tools/add-headers.py` adds
it). Protocol logic goes in a Rust crate, never in a wrapper (`CLAUDE.md`,
"Component philosophy").
