#!/usr/bin/env bash
# Copyright 2026 Anton Gravestam
# SPDX-License-Identifier: Apache-2.0
# Start a simulated Seyd robot on this Mac: the webcam through FFmpeg
# (sim/video-source.sh), the counter sensor (sim/sensor-source.py) and seydd,
# against the deployed cloud by default. Used for field-test run C (this Mac
# tethered to a phone hotspot, no network camera reachable).
#
#   ./sim-robot.sh                                   # robot id seyd-sim, webcam
#   VIDEO_DEVICE=lavfi ./sim-robot.sh                # synthetic motion source instead of the webcam
#   SIGNAL_URL=ws://localhost:8080/ws ./sim-robot.sh
#   DARC_QOS_PROFILE=latency ROBOT_ID=my-robot ./sim-robot.sh
#
# The config is a temp file, so there is nothing to point `seydd enrol` at.
# To join a fleet in the console, pass the enrolment token instead — it is
# redeemed once, against the same credential the daemon then runs with:
#
#   ENROL_TOKEN=seyd_enr_… SIGNAL_URL=ws://localhost:8080/ws ./sim-robot.sh
#
# Without it the robot trusts-on-first-use (dev signal servers only) and
# belongs to no org, so no signed-in console will see it.
set -euo pipefail
cd "$(dirname "$0")"
SIGNAL_URL="${SIGNAL_URL:-wss://seyd-signal-flj7s44j4a-ew.a.run.app/ws}"
PROFILE="${DARC_QOS_PROFILE:-balanced}"
ROBOT_ID="${ROBOT_ID:-seyd-sim}"
export DARC_QOS_PROFILE="$PROFILE"

CFG=$(mktemp -t seydd-sim.XXXXXX)
cat > "$CFG" <<TOML
[agent]
robot_id        = "$ROBOT_ID"
signal_url      = "$SIGNAL_URL"
credential_path = "$PWD/.seyd-sim.key"
qos_profile     = "$PROFILE"
max_sessions    = 4

[[channel]]
kind  = "video"
name  = "main"
input = "rtp://127.0.0.1:5000"
codec = "avc1.42001f"
fps   = 30

[[channel]]
kind  = "sensor"
name  = "telemetry"
input = "udp://127.0.0.1:5002"
codec = "json"

[[channel]]
kind   = "command"
name   = "ptz"
output = "udp://127.0.0.1:5004"
codec  = "json"

[publisher_control]
udp = "127.0.0.1:5003"
TOML
export PATH="$HOME/.cargo/bin:$PATH"
cargo build -p seydd --release --quiet
pkill -f "seydd --config" 2>/dev/null || true
pkill -f "ffmpeg.*rtp://127.0.0.1:5000" 2>/dev/null || true
pkill -f "sim/sensor-source.py" 2>/dev/null || true
trap 'kill 0 2>/dev/null; rm -f "$CFG"' EXIT
if [ -n "${ENROL_TOKEN:-}" ]; then
  ./target/release/seydd --config "$CFG" enrol --token "$ENROL_TOKEN"
fi
echo "robot $ROBOT_ID → $SIGNAL_URL (profile $PROFILE, video ${VIDEO_DEVICE:-webcam})"
./sim/video-source.sh &
python3 sim/sensor-source.py &
RUST_LOG="${RUST_LOG:-info}" ./target/release/seydd --config "$CFG" &
wait
