#!/usr/bin/env bash
# Bootstrap a fresh macOS checkout for Seyd development.
#
#   tools/setup-machine.sh
#
# Installs: Homebrew (if missing), Node 22 + pnpm, FFmpeg (for sim/), Rust via
# rustup (stable + rustfmt + clippy), the tools/.venv used by the browser test
# harnesses, and builds everything once.
set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"

if ! command -v brew >/dev/null; then
  /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
fi
brew list node@22 >/dev/null 2>&1 || brew install node@22
brew list ffmpeg >/dev/null 2>&1 || brew install ffmpeg
brew list python@3.12 >/dev/null 2>&1 || brew install python@3.12
command -v pnpm >/dev/null || npm install -g pnpm

if ! command -v "$HOME/.cargo/bin/cargo" >/dev/null; then
  curl -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path --profile minimal
fi
export PATH="$HOME/.cargo/bin:$PATH"
rustup component add rustfmt clippy

python3 -m venv "$REPO/tools/.venv"
"$REPO/tools/.venv/bin/pip" -q install --upgrade pip websockets

(cd "$REPO" && pnpm install --frozen-lockfile && pnpm -r build)
(cd "$REPO/cloud/api" && npm ci && npm run build)
(cd "$REPO" && cargo build --workspace)
chmod +x "$REPO/demo-seyd.sh" "$REPO/sim/video-source.sh" "$REPO/tools/"*.py

echo
echo "Ready. Next:"
echo "  cargo test --workspace && pnpm -r test          # unit + interop tests"
echo "  see CLAUDE.md 'Verifying work' for the local end-to-end run"
echo "  ./demo-seyd.sh                                  # the camera demo (needs .env.local)"
