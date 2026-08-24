#!/usr/bin/env bash
# Starts all DARC prototype processes locally for development.
# Prints the pilot URL — open it in a browser manually.
# Press Ctrl+C to stop everything.

set -uo pipefail

REPO="$(cd "$(dirname "$0")" && pwd)"
SIGNAL_PORT=8080
PILOT_PORT=3000
ROBOT_ID=mac-robot-01

# Set SIGNAL_URL to point the agent at a remote signal server.
# When unset, the local signal server is started and used.
SIGNAL_URL=${SIGNAL_URL:-}

PIDS=()

cleanup() {
  echo ""
  echo "Stopping all processes..."
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
  # Read loop rather than `sed`, which block-buffers when not writing to a
  # terminal and silently swallows subprocess output when this script is piped
  # to a file. See the same note in robot.sh.
  "$@" > >(while IFS= read -r line; do printf '  [%s] %s\n' "$label" "$line"; done) 2>&1 &
  local pid=$!
  PIDS+=("$pid")
  echo "(pid $pid)"
}

echo ""
echo "Starting DARC prototype..."
echo ""

if [[ -z "$SIGNAL_URL" ]]; then
  # Signaling server — must be up before the agent connects
  start "signal" env PORT="$SIGNAL_PORT" PILOT_URL="http://localhost:${PILOT_PORT}" node "$REPO/packages/signal/index.js"
  sleep 0.5
  AGENT_SIGNAL_URL="ws://localhost:${SIGNAL_PORT}"
else
  echo "  ↳ Using remote signal server: ${SIGNAL_URL}"
  AGENT_SIGNAL_URL="$SIGNAL_URL"
fi

# Robot simulation (not part of DARC — stand-ins for real robot publishers)
start "sensor-sim"  python3.12 "$REPO/sim/sensor-source.py"
start "video-sim"   bash "$REPO/sim/video-source.sh"

# DARC Agent — uses the venv python directly to avoid activation in a subshell
start "agent" "$REPO/packages/agent/.venv/bin/python" \
  "$REPO/packages/agent/agent.py" \
  --robot-id "$ROBOT_ID" \
  --signal-url "$AGENT_SIGNAL_URL" \
  --webtransport-host localhost

# Pilot static file server
start "pilot" npx --yes serve "$REPO/packages/pilot" --listen "$PILOT_PORT" --no-clipboard

if [[ -z "$SIGNAL_URL" ]]; then
  FLEET_URL="http://localhost:${SIGNAL_PORT}/"
else
  FLEET_URL="${SIGNAL_URL/wss:/https:}"
  FLEET_URL="${FLEET_URL/ws:/http:}"
  # Strip trailing path and add /
  FLEET_URL="${FLEET_URL%%/ws*}/"
fi

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""
echo "  Open in browser:"
echo ""
echo "  ${FLEET_URL}       ← fleet page (pick a vehicle)"
echo "  http://localhost:${PILOT_PORT}/        ← pilot (direct)"
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""
echo "  Ctrl+C to stop all processes."
echo ""

wait
