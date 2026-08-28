#!/usr/bin/env bash
# Starts the always-on demo robot: a real Hikvision PTZ camera over RTSP, with
# operator pan/tilt/zoom. See DEMO.md.
#
# Unlike robot.sh there is no sim/ here — no FFmpeg publisher and no sensor
# counter. The camera is a real video source that encodes for itself, so DARC
# pulls its RTSP stream directly and the publisher-control port has nobody
# listening on it. That is the whole point of this configuration: it is the first
# time the agent is fed by hardware rather than by a stand-in.
#
# Usage: ./demo.sh [robot-id]
#   CAMERA_IP=192.168.86.237        camera address (or set it in .env.local)
#   SIGNAL_URL=wss://...            override signal server
#   DARC_QOS_PROFILE=latency        latency | balanced | quality
#   PTZ_HOME=0,1800,10              elevation,azimuth,zoom to park at
#
# CAMERA_USER and CAMERA_PASSWORD come from .env.local, which is gitignored.
# They are exported into the agent's environment rather than passed as flags:
# anything on a command line is readable by any local process via `ps`.

set -uo pipefail

REPO="$(cd "$(dirname "$0")" && pwd)"
ROBOT_ID="${1:-${ROBOT_ID:-darc-demo}}"
SIGNAL_URL="${SIGNAL_URL:-wss://darc-signal-qjsonun6gq-ew.a.run.app}"
QOS_PROFILE="${DARC_QOS_PROFILE:-balanced}"
PTZ_HOME="${PTZ_HOME:-0,1800,10}"

# ── credentials ───────────────────────────────────────────────────────────────
if [[ -f "$REPO/.env.local" ]]; then
  set -a
  # shellcheck disable=SC1091
  . "$REPO/.env.local"
  set +a
fi

CAMERA_IP="${CAMERA_IP:-}"
CAMERA_USER="${CAMERA_USER:-admin}"
CAMERA_PASSWORD="${CAMERA_PASSWORD:-}"
CAMERA_CHANNEL="${CAMERA_CHANNEL:-101}"

if [[ -z "$CAMERA_IP" ]]; then
  echo "error: CAMERA_IP is not set." >&2
  echo "  Add it to .env.local, or run:  CAMERA_IP=192.168.x.x ./demo.sh" >&2
  echo "  Don't know the address?        tools/find-camera.py" >&2
  exit 1
fi
if [[ -z "$CAMERA_PASSWORD" ]]; then
  echo "error: CAMERA_PASSWORD is not set (expected in .env.local)." >&2
  exit 1
fi
export CAMERA_USER CAMERA_PASSWORD

# Credentials are injected by the agent from the environment, so the URL we
# build — and therefore anything that logs it — stays clean.
VIDEO_URL="rtsp://${CAMERA_IP}:554/Streaming/Channels/${CAMERA_CHANNEL}"

# The camera's sensor is PAL and caps at 25 fps. This only scales the backlog
# drop threshold, but claiming 30 would make it 20% too generous.
VIDEO_FPS="${VIDEO_FPS:-25}"

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
  # A read loop rather than `sed`, which block-buffers when its output is not a
  # terminal — see robot.sh.
  "$@" > >(while IFS= read -r line; do printf '  [%s] %s\n' "$label" "$line"; done) 2>&1 &
  local pid=$!
  PIDS+=("$pid")
  echo "(pid $pid)"
}

# ── preflight ─────────────────────────────────────────────────────────────────
# A camera that is unreachable, off, or wrong-passworded produces an agent that
# retries a connection forever with the reason buried in its log. Checking here
# turns that into one line before anything starts.
echo ""
echo "Checking camera at ${CAMERA_IP}..."
probe=$(curl -s -m 5 -o /dev/null -w '%{http_code}' --digest \
        -u "${CAMERA_USER}:${CAMERA_PASSWORD}" \
        "http://${CAMERA_IP}/ISAPI/System/deviceInfo" 2>/dev/null)
case "$probe" in
  200) echo "  ok — camera reachable and credentials accepted" ;;
  401) echo "  error: camera rejected ${CAMERA_USER} — check CAMERA_PASSWORD" >&2; exit 1 ;;
  000) echo "  error: no response from ${CAMERA_IP} — wrong address, or camera is off." >&2
       echo "         Try: tools/find-camera.py" >&2; exit 1 ;;
  403) echo "  error: camera returned 403. If it is factory-new it needs activating" >&2
       echo "         first — see 'Activating it' in DEMO.md." >&2; exit 1 ;;
  *)   echo "  error: unexpected HTTP ${probe} from camera" >&2; exit 1 ;;
esac

echo ""
echo "Starting demo robot: ${ROBOT_ID}"
echo "Signal:              ${SIGNAL_URL}"
echo "QoS profile:         ${QOS_PROFILE}  (transport half only — the camera"
echo "                     encodes for itself; see DEMO.md)"
echo "Camera:              ${CAMERA_IP} channel ${CAMERA_CHANNEL} @ ${VIDEO_FPS} fps"
echo "PTZ home:            ${PTZ_HOME}"
echo ""

pkill -f "agent.py" 2>/dev/null || true
sleep 0.5

start "agent" "$REPO/packages/agent/.venv/bin/python" \
  "$REPO/packages/agent/agent.py" \
  --robot-id "$ROBOT_ID" \
  --signal-url "$SIGNAL_URL" \
  --qos-profile "$QOS_PROFILE" \
  --video-url "$VIDEO_URL" \
  --video-fps "$VIDEO_FPS" \
  --camera-ip "$CAMERA_IP" \
  --ptz-home "$PTZ_HOME"

echo ""
echo "Demo robot is online. Open the fleet page to connect:"
echo "  ${SIGNAL_URL/wss:/https:}/"
echo ""
echo "  In the pilot:  DRAG the picture or ARROWS — look around"
echo "                 SHIFT — fast    WHEEL or +/− — zoom    H — home"
echo "                 SPACE — snapshot    S — stats overlay"
echo ""
echo "  Ctrl+C to stop."
echo ""

wait
