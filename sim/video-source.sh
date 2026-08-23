#!/usr/bin/env bash
# Captures the default Mac webcam and streams H.264 RTP to localhost:5000.
# This simulates what a robot's camera node publishes on its internal LAN.
# The DARC Agent subscribes to this port — start this before connecting a pilot.
set -euo pipefail

PORT=${VIDEO_PORT:-5000}
DEVICE=${VIDEO_DEVICE:-0}

echo "Available AVFoundation video devices:"
ffmpeg -f avfoundation -list_devices true -i "" 2>&1 | grep -A 40 "AVFoundation video devices" | grep -E "^\[|video" || true
echo ""
echo "Streaming webcam device [${DEVICE}] → RTP H.264 → UDP 127.0.0.1:${PORT}"
echo "Override device with VIDEO_DEVICE=1 if device 0 is not your camera."
echo "Press Ctrl+C to stop."
echo ""

ffmpeg \
  -fflags nobuffer \
  -f avfoundation \
  -framerate 30 \
  -video_size 1280x720 \
  -i "${DEVICE}" \
  -pix_fmt yuv420p \
  -vcodec libx264 \
  -tune zerolatency \
  -preset ultrafast \
  -profile:v baseline \
  -g 15 \
  -an \
  -flush_packets 1 \
  -f rtp \
  "rtp://127.0.0.1:${PORT}"
