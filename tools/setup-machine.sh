#!/usr/bin/env bash
# Bootstraps a fresh checkout of the DARC repo for local development.
#
# Works on both Apple Silicon (arm64, Homebrew at /opt/homebrew) and
# Intel (x86_64, Homebrew at /usr/local) Macs.
#
# Usage: ./tools/setup-machine.sh

set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "This script supports macOS only — the DARC prototype targets Mac-to-Mac." >&2
  exit 1
fi

ARCH="$(uname -m)"
if [[ "$ARCH" == "arm64" ]]; then
  BREW_PREFIX="/opt/homebrew"
elif [[ "$ARCH" == "x86_64" ]]; then
  BREW_PREFIX="/usr/local"
else
  echo "Unsupported architecture: $ARCH" >&2
  exit 1
fi
BREW_BIN="$BREW_PREFIX/bin/brew"

echo "DARC dev machine setup"
echo "======================"
echo "Architecture:    $ARCH"
echo "Homebrew prefix: $BREW_PREFIX"
echo ""

# --- Homebrew --------------------------------------------------------------

if [[ ! -x "$BREW_BIN" ]]; then
  echo "Homebrew not found — installing..."
  /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"

  # Persist brew on PATH for future shells (installer doesn't do this automatically).
  SHELL_PROFILE="$HOME/.zprofile"
  SHELLENV_LINE="eval \"\$($BREW_BIN shellenv)\""
  if ! grep -qsF "$SHELLENV_LINE" "$SHELL_PROFILE" 2>/dev/null; then
    echo "" >> "$SHELL_PROFILE"
    echo "$SHELLENV_LINE" >> "$SHELL_PROFILE"
    echo "Added Homebrew to $SHELL_PROFILE"
  fi
fi

eval "$("$BREW_BIN" shellenv)"

# --- System packages ---------------------------------------------------------

echo ""
echo "Installing Homebrew packages: node, python@3.12, ffmpeg..."
brew install node python@3.12 ffmpeg

PYTHON_BIN="$(brew --prefix python@3.12)/bin/python3.12"

# --- darc-agent (Python) ----------------------------------------------------

echo ""
echo "Setting up darc-agent virtualenv..."
"$PYTHON_BIN" -m venv "$REPO/packages/agent/.venv"
"$REPO/packages/agent/.venv/bin/pip" install --upgrade pip
"$REPO/packages/agent/.venv/bin/pip" install -r "$REPO/packages/agent/requirements.txt"

# --- darc-signal / darc-pilot (Node) ----------------------------------------

echo ""
echo "Installing darc-signal Node dependencies..."
(cd "$REPO/packages/signal" && npm install)

echo ""
echo "Installing darc-pilot Node dependencies..."
(cd "$REPO/packages/pilot" && npm install)

# --- Executable bits ---------------------------------------------------------

chmod +x "$REPO/dev.sh" "$REPO/robot.sh" "$REPO/sim/video-source.sh"

# --- Optional: gcloud (only needed to deploy darc-signal) ------------------

echo ""
if command -v gcloud >/dev/null 2>&1; then
  echo "gcloud CLI found: $(gcloud --version | head -1)"
else
  echo "gcloud CLI not found (optional — only needed to deploy darc-signal)."
  echo "  Install with: brew install --cask google-cloud-sdk"
  echo "  See packages/signal/DEPLOY.md"
fi

echo ""
echo "======================"
echo "Setup complete."
echo ""
echo "Versions:"
echo "  $(node --version 2>&1 | sed 's/^/  node    /')"
echo "  $("$REPO/packages/agent/.venv/bin/python" --version 2>&1 | sed 's/^/  python  /')"
echo "  $(ffmpeg -version 2>&1 | head -1 | sed 's/^/  ffmpeg  /')"
echo ""
echo "Next steps:"
echo "  ./dev.sh                            # run the full local prototype (signal + sim + agent + pilot)"
echo "  SIGNAL_URL=wss://... ./robot.sh      # run only the robot side, against a deployed signal server"
echo ""
echo "Note: the first time FFmpeg captures the webcam, macOS will prompt for camera access — grant it to the Terminal app."
