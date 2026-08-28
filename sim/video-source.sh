#!/usr/bin/env bash
# Captures the default Mac webcam and streams H.264 RTP to localhost:5000.
# This simulates what a robot's camera node publishes on its internal LAN.
# The DARC Agent subscribes to this port — start this before connecting a pilot.
#
# NOT part of DARC. This stands in for the robot's video publisher, and it owns
# the encoder settings: per SPEC.md, DARC states a bitrate ceiling and a latency
# budget, and the publisher decides how to meet them (resolution, preset, VBV,
# GOP). That is why the resolution table lives here and not in packages/seyd-qos (the Seyd half).
set -euo pipefail

PORT=${VIDEO_PORT:-5000}
DEVICE=${VIDEO_DEVICE:-0}
PROFILE=${DARC_QOS_PROFILE:-balanced}

# ── QoS profile → encoder settings ───────────────────────────────────────────
# Bitrate ceilings mirror packages/seyd-qos (the Seyd half). Keep the two in step: DARC sizes
# its FEC overhead against these numbers, and the pair has to fit the uplink as
# one budget.
case "$PROFILE" in
  latency)
    W=960;  H=540; FPS=30; KBPS=1500; GOP=30; PRESET=ultrafast; VBV_MS=100 ;;
  balanced)
    W=1280; H=720; FPS=30; KBPS=3000; GOP=30; PRESET=veryfast;  VBV_MS=100 ;;
  quality)
    W=1280; H=720; FPS=30; KBPS=6000; GOP=60; PRESET=veryfast;  VBV_MS=200 ;;
  *)
    echo "Unknown DARC_QOS_PROFILE '${PROFILE}' — use latency, balanced, or quality." >&2
    exit 1 ;;
esac

# VBV buffer = how long the encoder may run over its average rate. This is a
# latency knob, not just a quality one:
#   • ffmpeg's default (bufsize == maxrate, i.e. 1s) lets a motion burst emit a
#     whole second of extra bits. The modem queue absorbs that as ~1s of added
#     glass-to-glass latency, or drops it as the packet loss we are fixing.
#   • One frame (33ms) is too tight — an IDR legitimately needs 3-5x an average
#     frame, so a one-frame buffer forces keyframe QP up brutally and the
#     keyframe is the frame you least want ugly.
#   • ~100ms (3 frames) fits one IDR without allowing a deep queue.
BUFK=$(( KBPS * VBV_MS / 1000 ))

echo "DARC video publisher"
echo "  profile   ${PROFILE}"
echo "  video     ${W}x${H} @ ${FPS}fps"
echo "  bitrate   ${KBPS} kbps capped (VBV ${BUFK}k = ${VBV_MS}ms)"
echo "  GOP       ${GOP} frames ($(( GOP * 1000 / FPS ))ms)"
echo "  preset    ${PRESET}"
echo ""

# ── input ────────────────────────────────────────────────────────────────────
# Capture resolution and encode resolution are separate concerns. A camera only
# offers a handful of discrete capture modes, and the profile's target is rarely
# one of them — 960x540 is not a mode any Mac webcam supports, for instance.
# Asking AVFoundation for an unsupported size does not fall back, it refuses to
# open the device at all: no camera light, no frames, and the agent just reports
# an RTP read timeout with nothing to explain it. So capture at a real mode and
# scale to the target, which is also what a real robot camera pipeline does.
detect_capture_mode() {
  # Requesting an impossible size makes ffmpeg print the device's actual mode
  # list. There is no cleaner way to enumerate them.
  local modes
  modes=$(ffmpeg -hide_banner -f avfoundation -video_size 1x1 -i "${DEVICE}" \
            -t 0 -f null - 2>&1 \
          | sed -n 's/^.*[[:space:]]\([0-9]\{2,\}x[0-9]\{2,\}\)@.*$/\1/p' | sort -u)
  [[ -z "$modes" ]] && return 1

  # Smallest mode that still covers the target, so we scale down and never up.
  local best="" best_px=0 mw mh px
  while read -r m; do
    mw=${m%x*}; mh=${m#*x}
    (( mw < W || mh < H )) && continue
    px=$(( mw * mh ))
    if [[ -z "$best" ]] || (( px < best_px )); then best=$m; best_px=$px; fi
  done <<< "$modes"

  # Nothing large enough — take the biggest on offer and accept the upscale.
  if [[ -z "$best" ]]; then
    while read -r m; do
      mw=${m%x*}; mh=${m#*x}; px=$(( mw * mh ))
      if (( px > best_px )); then best=$m; best_px=$px; fi
    done <<< "$modes"
  fi
  echo "$best"
}

VIDEO_FILTER=()
if [[ "$DEVICE" == "lavfi" ]]; then
  echo "Input: synthetic testsrc2 (reproducible motion fixture)"
  # -re is essential here and only here. A webcam is paced by hardware, but a
  # lavfi source generates frames as fast as the CPU allows — measured at ~2450
  # fps / 157 Mbps without it, which floods the relay and makes every latency and
  # loss number meaningless.
  INPUT_ARGS=(-re -f lavfi -i "testsrc2=size=${W}x${H}:rate=${FPS}")
else
  CAPTURE="${CAPTURE_SIZE:-$(detect_capture_mode || true)}"
  if [[ -z "$CAPTURE" ]]; then
    CAPTURE="1280x720"
    echo "Could not enumerate camera modes — falling back to ${CAPTURE}."
    echo "Override with CAPTURE_SIZE=WxH if the camera rejects it."
  fi
  echo "Input: webcam device [${DEVICE}], capturing ${CAPTURE}"
  echo "       (VIDEO_DEVICE=1 for another camera, =lavfi for synthetic)"
  INPUT_ARGS=(-f avfoundation -framerate "${FPS}" -video_size "${CAPTURE}" -i "${DEVICE}")
  if [[ "$CAPTURE" != "${W}x${H}" ]]; then
    echo "       scaling ${CAPTURE} → ${W}x${H} for the ${PROFILE} profile"
    VIDEO_FILTER=(-vf "scale=${W}:${H}")
  fi
fi

echo "Streaming → RTP H.264 → UDP 127.0.0.1:${PORT}"
echo "Press Ctrl+C to stop."
echo ""

# ── encode ───────────────────────────────────────────────────────────────────
# scenecut=0 forces strictly periodic IDRs. A scene-cut IDR is an unpredictable
# bitrate spike, and on a rate-limited link an unpredictable spike is exactly
# what we are eliminating. It also keeps DARC's per-keyframe FEC accounting
# predictable.
#
# Deliberately NOT using nal-hrd=cbr: it pads to hit the rate exactly, spending
# scarce uplink on filler bytes.
exec ffmpeg \
  -fflags nobuffer \
  "${INPUT_ARGS[@]}" \
  "${VIDEO_FILTER[@]}" \
  -pix_fmt yuv420p \
  -c:v libx264 \
  -tune zerolatency \
  -preset "${PRESET}" \
  -profile:v baseline \
  -b:v "${KBPS}k" -maxrate "${KBPS}k" -bufsize "${BUFK}k" \
  -g "${GOP}" -keyint_min "${GOP}" -bf 0 \
  -x264-params "scenecut=0" \
  -an \
  -flush_packets 1 \
  -max_delay 0 \
  -f rtp "rtp://127.0.0.1:${PORT}"
