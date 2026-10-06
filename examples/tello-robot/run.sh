#!/usr/bin/env bash
# Copyright 2026 Anton Gravestam
# SPDX-License-Identifier: Apache-2.0
# Start the Tello demo robot: seydd (generic daemon) + bridge.py (the drone).
#
#   examples/tello-robot/run.sh                          # drone on Wi-Fi, deployed cloud
#   SIGNAL_URL=ws://localhost:8080/ws examples/tello-robot/run.sh
#   TELLO_IP=127.0.0.1 examples/tello-robot/run.sh       # against fake_tello.py
#   ENROL_TOKEN=seyd_enr_… examples/tello-robot/run.sh   # first start: enrol the robot's key
#   HOST=rust examples/tello-robot/run.sh                # the native host (host/) instead of bridge.py + seydd
#
# Overrides: SIGNAL_URL, ROBOT_ID (seyd-tello), DARC_QOS_PROFILE (latency),
# TELLO_IP (192.168.10.1), TELLO_ALT_LIMIT_M (5), BRIDGE_ARGS (e.g. --no-takeoff),
# RUST_LOG. The top-level ./demo-tello.sh adds the network preflight.
set -euo pipefail
cd "$(dirname "$0")/../.."
if [ -f .env.local ]; then set -a; . ./.env.local; set +a; fi
SIGNAL_URL="${SIGNAL_URL:-wss://seyd-signal-flj7s44j4a-ew.a.run.app/ws}"
ROBOT_ID="${ROBOT_ID:-seyd-tello}"
PROFILE="${DARC_QOS_PROFILE:-latency}"
TELLO_IP="${TELLO_IP:-192.168.10.1}"

CFG=$(mktemp -t seydd.XXXXXX)
sed -E "s#^signal_url *=.*#signal_url = \"$SIGNAL_URL\"#; \
        s#^robot_id *=.*#robot_id = \"$ROBOT_ID\"#; \
        s#^qos_profile *=.*#qos_profile = \"$PROFILE\"#" examples/tello-robot/seydd.toml > "$CFG"
export PATH="$HOME/.cargo/bin:$PATH"
if [ "${HOST:-python}" = rust ]; then
  cargo build -p tello-host --release --quiet
  pkill -f "seydd --config" 2>/dev/null || true
  pkill -f "tello-robot/bridge.py" 2>/dev/null || true
  pkill -f "target/release/tello-host" 2>/dev/null || true
  echo "robot $ROBOT_ID → $SIGNAL_URL (profile $PROFILE), drone $TELLO_IP — native host"
  rm -f "$CFG"
  # shellcheck disable=SC2086
  exec env RUST_LOG="${RUST_LOG:-info}" ./target/release/tello-host --drone-ip "$TELLO_IP" --signal-url "$SIGNAL_URL" \
       --robot-id "$ROBOT_ID" --qos-profile "$PROFILE" ${BRIDGE_ARGS:-}
fi
cargo build -p seydd --release --quiet
pkill -f "target/release/tello-host" 2>/dev/null || true

if [ -n "${ENROL_TOKEN:-}" ]; then
  ./target/release/seydd --config "$CFG" enrol --token "$ENROL_TOKEN"
fi

pkill -f "seydd --config" 2>/dev/null || true
pkill -f "tello-robot/bridge.py" 2>/dev/null || true
trap 'kill 0 2>/dev/null; rm -f "$CFG"' EXIT
echo "robot $ROBOT_ID → $SIGNAL_URL (profile $PROFILE), drone $TELLO_IP"
# shellcheck disable=SC2086
python3 examples/tello-robot/bridge.py --drone-ip "$TELLO_IP" ${BRIDGE_ARGS:-} &
RUST_LOG="${RUST_LOG:-info}" ./target/release/seydd --config "$CFG" &
wait
