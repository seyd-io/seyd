#!/usr/bin/env bash
# Find the Hikvision demo camera and bring the demo robot up on the deployed
# cloud — the whole "find a camera and start the demo" procedure in one go.
#
#   ./demo-start.sh            # find camera, start demo, stay attached (Ctrl-C stops it)
#   ./demo-start.sh --detach   # same, but leave the demo running and return
#   CAMERA_IP=… ./demo-start.sh   # skip discovery, use this address
#
# What it does, in order (each step is what an operator would do by hand):
#   1. Probe the camera at CAMERA_IP (CLI, then .env.local) over ISAPI.
#   2. If that fails, run tools/find-camera.py (SADP + ONVIF on every
#      interface) and take the first candidate that accepts our credentials.
#   3. Start ./demo-seyd.sh with that address, logging to $DEMO_LOG.
#   4. Wait until the signal server lists the robot as online, then print
#      the pilot URL.
#
# Reads .env.local for CAMERA_USER / CAMERA_PASSWORD / CAMERA_IP. Same
# overrides as demo-seyd.sh: SIGNAL_URL, DARC_QOS_PROFILE, ROBOT_ID, RUST_LOG.
set -euo pipefail
cd "$(dirname "$0")"

DETACH=0
for arg in "$@"; do
  case "$arg" in
    --detach) DETACH=1 ;;
    -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
    *) echo "unknown argument: $arg" >&2; exit 2 ;;
  esac
done

CLI_CAMERA_IP="${CAMERA_IP:-}"
if [ -f .env.local ]; then set -a; . ./.env.local; set +a; fi
if [ -n "$CLI_CAMERA_IP" ]; then CAMERA_IP="$CLI_CAMERA_IP"; fi
: "${CAMERA_PASSWORD:?set CAMERA_PASSWORD in .env.local}"
CAMERA_USER="${CAMERA_USER:-admin}"
SIGNAL_URL="${SIGNAL_URL:-wss://seyd-signal-flj7s44j4a-ew.a.run.app/ws}"
ROBOT_ID="${ROBOT_ID:-seyd-demo}"
DEMO_LOG="${DEMO_LOG:-${TMPDIR:-/tmp}/seyd-demo.log}"
# ws(s)://host/ws → http(s)://host
API_BASE="${SIGNAL_URL%/ws}"; API_BASE="${API_BASE/#wss:/https:}"; API_BASE="${API_BASE/#ws:/http:}"

# ── 1. probe: does this address answer ISAPI with our credentials? ─────────
probe() {  # probe <ip> → prints HTTP code (000 = unreachable)
  curl -s -o /dev/null -w '%{http_code}' --digest -u "$CAMERA_USER:$CAMERA_PASSWORD" \
       --max-time 4 "http://$1/ISAPI/System/deviceInfo" || true   # curl prints 000 itself on failure
}

FOUND=""
if [ -n "${CAMERA_IP:-}" ]; then
  code=$(probe "$CAMERA_IP")
  case "$code" in
    200) FOUND="$CAMERA_IP"; echo "camera $CAMERA_IP: ok (configured address)" ;;
    401) echo "camera $CAMERA_IP: wrong credentials (401)"; exit 1 ;;
    403) echo "camera $CAMERA_IP: not activated (403) — activate it in its web UI first, see DEMO.md"; exit 1 ;;
    *)   echo "camera $CAMERA_IP: no answer ($code) — running discovery" ;;
  esac
fi

# ── 2. discover: SADP + ONVIF on every interface, take the first that answers ─
if [ -z "$FOUND" ]; then
  echo "discovering cameras (tools/find-camera.py)..."
  candidates=$(python3 tools/find-camera.py --timeout "${DISCOVERY_TIMEOUT:-8}" \
               | awk '/^  [0-9]+\.[0-9]+\.[0-9]+\.[0-9]+ +via /{print $1}')
  if [ -z "$candidates" ]; then
    echo "no camera found. Same switch as this machine? PoE lit? Try: tools/find-camera.py --scan" >&2
    exit 1
  fi
  for ip in $candidates; do
    code=$(probe "$ip")
    case "$code" in
      200) FOUND="$ip"; echo "camera $ip: ok (discovered)"; break ;;
      401) echo "camera $ip: found but rejects our credentials (401)" ;;
      403) echo "camera $ip: found but not activated (403)" ;;
      *)   echo "camera $ip: found by discovery but ISAPI gives $code" ;;
    esac
  done
  [ -n "$FOUND" ] || { echo "found camera(s) but none accepts CAMERA_USER/CAMERA_PASSWORD" >&2; exit 1; }
  if [ "$FOUND" != "${CAMERA_IP:-}" ]; then
    echo "note: camera moved — update CAMERA_IP=$FOUND in .env.local to skip discovery next time"
  fi
fi

# ── 3. start the demo robot ─────────────────────────────────────────────────
: > "$DEMO_LOG"
echo "starting demo robot → log: $DEMO_LOG"
if [ "$DETACH" = 1 ]; then
  CAMERA_IP="$FOUND" nohup ./demo-seyd.sh >"$DEMO_LOG" 2>&1 &
else
  CAMERA_IP="$FOUND" ./demo-seyd.sh >"$DEMO_LOG" 2>&1 &
  trap 'kill 0 2>/dev/null' EXIT INT TERM
fi
DEMO_PID=$!

# ── 4. wait until the cloud lists the robot online ──────────────────────────
online=0
for _ in $(seq 1 "${ONLINE_TIMEOUT:-60}"); do
  if ! kill -0 "$DEMO_PID" 2>/dev/null; then
    echo "demo-seyd.sh exited early:"; tail -20 "$DEMO_LOG"; exit 1
  fi
  if grep -qE 'unknown-robot|denied|panicked' "$DEMO_LOG"; then
    echo "robot rejected by the signal server:"; grep -E 'unknown-robot|denied|panicked' "$DEMO_LOG" | head -3
    echo "  (the robot's key must be enrolled — see CLAUDE.md § Hosting)"; exit 1
  fi
  # Our own instance must have connected (a previous instance draining on the
  # server would otherwise still show the robot online) and the cloud must list it.
  grep -q 'signal connected' "$DEMO_LOG" || { sleep 1; continue; }
  if curl -s --max-time 5 "$API_BASE/api/v1/robots" \
       | python3 -c "import json,sys; rs=json.load(sys.stdin)['robots']; sys.exit(0 if any(r['robot_id']=='$ROBOT_ID' and r['online'] for r in rs) else 1)" 2>/dev/null; then
    online=1; break
  fi
  sleep 1
done

if [ "$online" = 1 ]; then
  echo "robot $ROBOT_ID online at $API_BASE"
  echo "pilot: $API_BASE/pilot/?robot=$ROBOT_ID   (press S for the HUD)"
  echo "verify: tools/.venv/bin/python3 tools/seyd-smoke.py --robot $ROBOT_ID --no-sensor --camera-ip $FOUND"
else
  echo "robot $ROBOT_ID not listed online after ${ONLINE_TIMEOUT:-60}s — last log lines:"; tail -20 "$DEMO_LOG"
  [ "$DETACH" = 1 ] || exit 1
fi

if [ "$DETACH" = 1 ]; then
  echo "left running (pid $DEMO_PID); stop with: pkill -f 'seydd --config'; pkill -f demo-robot/bridge.py"
else
  echo "attached — Ctrl-C stops the demo"
  tail -n +1 -f "$DEMO_LOG" &
  wait "$DEMO_PID"
fi
