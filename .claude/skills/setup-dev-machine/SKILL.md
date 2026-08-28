---
name: setup-dev-machine
description: Use when the user wants to prepare a freshly checked-out Seyd repo for local development on a new Mac — Homebrew, Node 22 + pnpm, FFmpeg, Rust via rustup, the tools/.venv for the browser harnesses, and a first build. Triggers on "install all tools needed and set up this machine for development", "bootstrap this machine", "set up my dev environment", "get Seyd running on a new Mac".
---

Run `tools/setup-machine.sh` from the repo root and report what it installed.
It is idempotent. It installs Homebrew if missing, `node@22`, `pnpm`, `ffmpeg`,
`python@3.12`, Rust via rustup (`--no-modify-path`, so add `~/.cargo/bin` to
PATH), creates `tools/.venv` with `websockets`, then runs `pnpm install`,
`pnpm -r build`, `npm ci && npm run build` in `cloud/api`, and
`cargo build --workspace`.

Afterwards verify with `cargo test --workspace` and `pnpm -r test`, and point
the user at CLAUDE.md "Verifying work" for the local end-to-end run and at
`./demo-seyd.sh` for the camera demo (needs `.env.local`).
