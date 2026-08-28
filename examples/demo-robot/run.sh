#!/usr/bin/env bash
# Start the Seyd demo robot: seydd (generic daemon) + bridge.py (Hikvision).
#
#   examples/demo-robot/run.sh                       # signal from seydd.toml
#   SIGNAL_URL=ws://localhost:8080/ws examples/demo-robot/run.sh
#
# Reads .env.local (CAMERA_USER / CAMERA_PASSWORD, optional CAMERA_IP).
set -euo pipefail
cd "$(dirname "$0")/../.."
if [ -f .env.local ]; then set -a; . ./.env.local; set +a; fi
: "${CAMERA_PASSWORD:?set CAMERA_PASSWORD in .env.local}"
export SEYD_RTSP_USER="${CAMERA_USER:-admin}" SEYD_RTSP_PASSWORD="$CAMERA_PASSWORD"
CFG=examples/demo-robot/seydd.toml
if [ -n "${SIGNAL_URL:-}" ]; then
  TMP=$(mktemp -t seydd.XXXXXX.toml)
  sed -E "s#^signal_url *=.*#signal_url = \"$SIGNAL_URL\"#" "$CFG" > "$TMP"; CFG=$TMP
fi
if [ -n "${CAMERA_IP:-}" ]; then
  TMP2=$(mktemp -t seydd.XXXXXX.toml)
  sed -E "s#rtsp://[0-9.]+:554#rtsp://$CAMERA_IP:554#" "$CFG" > "$TMP2"; CFG=$TMP2
fi
export PATH="$HOME/.cargo/bin:$PATH"
cargo build -p seydd --release >/dev/null
pkill -f "seydd --config" 2>/dev/null || true
pkill -f "demo-robot/bridge.py" 2>/dev/null || true
trap 'kill 0' EXIT
python3 examples/demo-robot/bridge.py ${CAMERA_IP:+--camera-ip "$CAMERA_IP"} &
RUST_LOG="${RUST_LOG:-info}" ./target/release/seydd --config "$CFG" &
wait
