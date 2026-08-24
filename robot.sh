#!/usr/bin/env bash
# Starts the robot-side processes only (FFmpeg + sensor-sim + agent).
# Connects to the cloud signal server by default.
# Usage: ./robot.sh [robot-id]
#   SIGNAL_URL=wss://...          override signal server
#   ROBOT_ID=my-robot             override robot ID
#   DARC_QOS_PROFILE=latency      latency | balanced | quality
#   VIDEO_DEVICE=lavfi            synthetic motion fixture instead of the webcam

set -uo pipefail

REPO="$(cd "$(dirname "$0")" && pwd)"
ROBOT_ID="${1:-${ROBOT_ID:-mac-robot-01}}"
SIGNAL_URL="${SIGNAL_URL:-wss://darc-signal-qjsonun6gq-ew.a.run.app}"
# Both halves of a QoS profile come from one variable: the publisher reads it
# directly, and it is passed to the agent so its FEC and drop policy match. They
# have to be set together — FEC overhead and video bitrate are one budget.
QOS_PROFILE="${DARC_QOS_PROFILE:-balanced}"

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
  # A read loop rather than `sed`: sed block-buffers when its output is not a
  # terminal, so piping this script to a file silently swallowed subprocess
  # output — including the ffmpeg error explaining why the camera never opened.
  # Process substitution keeps $! pointing at the command, not the filter.
  "$@" > >(while IFS= read -r line; do printf '  [%s] %s\n' "$label" "$line"; done) 2>&1 &
  local pid=$!
  PIDS+=("$pid")
  echo "(pid $pid)"
}

echo ""
echo "Starting robot: ${ROBOT_ID}"
echo "Signal:         ${SIGNAL_URL}"
echo "QoS profile:    ${QOS_PROFILE}"
echo "Video source:   ${VIDEO_DEVICE:-0 (webcam)}"
echo ""

# Kill any stale agent / ffmpeg / sensor-sim from a previous run so ports are free.
pkill -f "agent.py" 2>/dev/null || true
pkill -f "sensor-source.py" 2>/dev/null || true
pkill -f "video-source.sh" 2>/dev/null || true
pkill -f "ffmpeg.*rtp" 2>/dev/null || true
sleep 0.5

start "sensor-sim"  python3.12 "$REPO/sim/sensor-source.py"
start "video-sim"   env DARC_QOS_PROFILE="$QOS_PROFILE" bash "$REPO/sim/video-source.sh"
start "agent"       "$REPO/packages/agent/.venv/bin/python" \
  "$REPO/packages/agent/agent.py" \
  --robot-id "$ROBOT_ID" \
  --signal-url "$SIGNAL_URL" \
  --qos-profile "$QOS_PROFILE"

echo ""
echo "Robot is online. Open the fleet page to connect:"
echo "  ${SIGNAL_URL/wss:/https:}/"
echo ""
echo "  In the pilot:  S — stats overlay    SPACE — snapshot"
echo "  Red canvas border = unrecoverable loss; picture is not trustworthy"
echo "  until the next clean keyframe."
echo ""
echo "  Ctrl+C to stop."
echo ""

wait
