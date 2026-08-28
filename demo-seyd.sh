#!/usr/bin/env bash
# Start the always-on Seyd demo robot (Hikvision PTZ, DEMO.md) on the Seyd
# stack: seydd (generic daemon) + examples/demo-robot/bridge.py (the
# camera-specific part). The successor of demo.sh.
#
#   ./demo-seyd.sh                                   # against the deployed cloud
#   SIGNAL_URL=ws://localhost:8080/ws ./demo-seyd.sh # against a local cloud/api
#   CAMERA_IP=192.168.86.237 ./demo-seyd.sh
#   DARC_QOS_PROFILE=latency ./demo-seyd.sh          # QoS ceiling (latency|balanced|quality)
#   RUST_LOG=debug ./demo-seyd.sh
#
# Reads .env.local for CAMERA_USER / CAMERA_PASSWORD (and optional CAMERA_IP).
set -euo pipefail
cd "$(dirname "$0")"
if [ -f .env.local ]; then set -a; . ./.env.local; set +a; fi
: "${CAMERA_PASSWORD:?set CAMERA_PASSWORD in .env.local}"
CAMERA_IP="${CAMERA_IP:-192.168.86.237}"
SIGNAL_URL="${SIGNAL_URL:-wss://seyd-signal-flj7s44j4a-ew.a.run.app/ws}"
PROFILE="${DARC_QOS_PROFILE:-balanced}"
ROBOT_ID="${ROBOT_ID:-seyd-demo}"

# ── camera preflight: one clear line instead of a daemon retrying forever ──
code=$(curl -s -o /dev/null -w '%{http_code}' --digest -u "${CAMERA_USER:-admin}:$CAMERA_PASSWORD" \
       --max-time 5 "http://$CAMERA_IP/ISAPI/System/deviceInfo" || true)
case "$code" in
  200) echo "camera $CAMERA_IP: ok" ;;
  401) echo "camera $CAMERA_IP: wrong credentials (401)"; exit 1 ;;
  403) echo "camera $CAMERA_IP: not activated (403) — activate it in its web UI first, see DEMO.md"; exit 1 ;;
  000) echo "camera $CAMERA_IP: unreachable — is it on this LAN? try tools/find-camera.py"; exit 1 ;;
  *)   echo "camera $CAMERA_IP: unexpected HTTP $code"; exit 1 ;;
esac

# ── seydd config derived from the checked-in one ────────────────────────────
CFG=$(mktemp -t seydd.XXXXXX)
sed -E "s#^signal_url *=.*#signal_url = \"$SIGNAL_URL\"#; \
        s#^robot_id *=.*#robot_id = \"$ROBOT_ID\"#; \
        s#^qos_profile *=.*#qos_profile = \"$PROFILE\"#; \
        s#rtsp://[0-9.]+:554#rtsp://$CAMERA_IP:554#" examples/demo-robot/seydd.toml > "$CFG"
export SEYD_RTSP_USER="${CAMERA_USER:-admin}" SEYD_RTSP_PASSWORD="$CAMERA_PASSWORD"
export PATH="$HOME/.cargo/bin:$PATH"
cargo build -p seydd --release --quiet

pkill -f "seydd --config" 2>/dev/null || true
pkill -f "demo-robot/bridge.py" 2>/dev/null || true
trap 'kill 0 2>/dev/null; rm -f "$CFG"' EXIT
echo "robot $ROBOT_ID → $SIGNAL_URL (profile $PROFILE)"
python3 examples/demo-robot/bridge.py --camera-ip "$CAMERA_IP" &
RUST_LOG="${RUST_LOG:-info}" ./target/release/seydd --config "$CFG" &
wait
