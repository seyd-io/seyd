#!/usr/bin/env bash
# Starts the robot-side processes only (FFmpeg + sensor-sim + agent).
# Connects to the cloud signal server by default.
# Usage: ./robot.sh [robot-id]
#   SIGNAL_URL=wss://... ./robot.sh    override signal server
#   ROBOT_ID=my-robot   ./robot.sh    override robot ID

set -uo pipefail

REPO="$(cd "$(dirname "$0")" && pwd)"
ROBOT_ID="${1:-${ROBOT_ID:-mac-robot-01}}"
SIGNAL_URL="${SIGNAL_URL:-wss://darc-signal-qjsonun6gq-ew.a.run.app}"

PIDS=()

cleanup() {
  echo ""
  echo "Stopping..."
  for pid in "${PIDS[@]}"; do
    kill "$pid" 2>/dev/null || true
  done
  wait 2>/dev/null || true
  echo "Done."
}
trap cleanup EXIT INT TERM

start() {
  local label="$1"; shift
  printf "  ▶ %-20s" "$label"
  "$@" > >(sed "s/^/  [${label}] /") 2>&1 &
  local pid=$!
  PIDS+=("$pid")
  echo "(pid $pid)"
}

echo ""
echo "Starting robot: ${ROBOT_ID}"
echo "Signal:         ${SIGNAL_URL}"
echo ""

# Kill any stale agent / ffmpeg / sensor-sim from a previous run so ports are free.
pkill -f "agent.py" 2>/dev/null || true
pkill -f "sensor-source.py" 2>/dev/null || true
pkill -f "video-source.sh" 2>/dev/null || true
pkill -f "ffmpeg.*rtp" 2>/dev/null || true
sleep 0.5

start "sensor-sim"  python3.12 "$REPO/sim/sensor-source.py"
start "video-sim"   bash "$REPO/sim/video-source.sh"
start "agent"       "$REPO/packages/agent/.venv/bin/python" \
  "$REPO/packages/agent/agent.py" \
  --robot-id "$ROBOT_ID" \
  --signal-url "$SIGNAL_URL"

echo ""
echo "Robot is online. Open the fleet page to connect:"
echo "  ${SIGNAL_URL/wss:/https:}/"
echo ""
echo "  Ctrl+C to stop."
echo ""

wait
