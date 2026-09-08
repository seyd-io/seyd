#!/usr/bin/env bash
# Bootstrap a fresh macOS checkout for Seyd development.
#
#   tools/setup-machine.sh
#
# Installs: Homebrew (if missing), Node 22 + pnpm, FFmpeg (for sim/), Rust via
# rustup (stable + rustfmt + clippy), Colima + the Docker CLI (for the cloud
# compose stack), the tools/.venv used by the browser test harnesses, and
# builds everything once.
#
# Colima rather than Docker Desktop: it gives a real Docker daemon headlessly,
# with no GUI, no licence prompt and no admin password. `colima start` is left
# to you — it allocates a VM, so it should be a deliberate act.
set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"

if ! command -v brew >/dev/null; then
  /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
fi
brew list node@22 >/dev/null 2>&1 || brew install node@22
brew list ffmpeg >/dev/null 2>&1 || brew install ffmpeg
brew list python@3.12 >/dev/null 2>&1 || brew install python@3.12
command -v pnpm >/dev/null || npm install -g pnpm

# Container runtime for `cloud/docker-compose.yml` (Postgres + Logto + api).
brew list colima >/dev/null 2>&1 || brew install colima
brew list docker >/dev/null 2>&1 || brew install docker
brew list docker-compose >/dev/null 2>&1 || brew install docker-compose
# Homebrew installs Compose as a plugin Docker cannot find on its own.
mkdir -p "$HOME/.docker/cli-plugins"
ln -sf /opt/homebrew/lib/docker/cli-plugins/docker-compose "$HOME/.docker/cli-plugins/docker-compose"

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
echo
echo "For the cloud (console login, Postgres, Logto):"
echo "  colima start --cpu 4 --memory 6 --disk 20       # once per boot; needs ~6 GB free"
echo "  (cd cloud && docker compose up -d)              # then see cloud/README.md"
