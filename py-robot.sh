#!/usr/bin/env bash
# Copyright 2026 Anton Gravestam
# SPDX-License-Identifier: Apache-2.0
# Start a Seyd robot built on the Python SDK: x264 through a pipe, access units
# pushed straight into libseyd over the C ABI (ADR 0004). No seydd, no RTP
# socket — this is the archetype where the robot's own process already holds
# encoded frames. sdks/python/examples/ffmpeg_robot.py is the program; this
# script only makes sure the prerequisites exist and then runs it.
#
#   ./py-robot.sh                                    # robot id seyd-py, synthetic source
#   VIDEO_DEVICE=0 ./py-robot.sh                     # the Mac webcam instead
#   DARC_QOS_PROFILE=latency ROBOT_ID=my-py ./py-robot.sh
#   SIGNAL_URL=ws://localhost:8080/ws ./py-robot.sh  # against a local cloud
#   ENROL_TOKEN=seyd_enr_… ./py-robot.sh             # first run on a cloud with real accounts
#
# ENROL_TOKEN redeems a one-time enrolment token from the console into the
# same credential file the robot then runs with, exactly as sim-robot.sh does.
# The C ABI has no enrolment call yet (PLAN.md §2.3 lists it under
# `seyd_config`), so the redemption goes through `seydd enrol`; the key file
# format is shared because both hosts use seyd_signal_client::Identity.
#
# Then open the printed pilot URL in Chrome (the pilot is Chromium-only — see
# PLAN.md open question 5) and press S for the HUD. On the deployed cloud a
# same-LAN pilot falls through to the srflx hairpin unless you grant Chrome's
# Local Network Access prompt, so expect ~12 ms rather than <1 ms there.
set -euo pipefail
cd "$(dirname "$0")"

SIGNAL_URL="${SIGNAL_URL:-wss://seyd-signal-flj7s44j4a-ew.a.run.app/ws}"
PROFILE="${DARC_QOS_PROFILE:-balanced}"
ROBOT_ID="${ROBOT_ID:-seyd-py}"
DEVICE="${VIDEO_DEVICE:-lavfi}"
VENV="tools/.venv"
PY="$VENV/bin/python3"

# ── prerequisites ────────────────────────────────────────────────────────────
# Each one is checked rather than assumed, because the failure modes otherwise
# land far from the cause: a missing libseyd surfaces as an import error inside
# cffi, and a missing ffmpeg as an empty pipe with no frames and no message.

command -v ffmpeg >/dev/null || {
  echo "ffmpeg not found — the robot has no encoder. brew install ffmpeg" >&2
  exit 1
}

export PATH="$HOME/.cargo/bin:$PATH"
command -v cargo >/dev/null || {
  echo "cargo not found — run tools/setup-machine.sh" >&2
  exit 1
}

# libseyd is what the SDK dlopens; sdks/python/seyd/_ffi.py finds it in
# target/release without an install step.
echo "building libseyd…"
cargo build -p seyd-ffi --release --quiet

if [[ ! -x "$PY" ]]; then
  echo "creating $VENV…"
  python3 -m venv "$VENV"
fi
"$PY" -c 'import cffi' 2>/dev/null || {
  echo "installing cffi into $VENV…"
  "$PY" -m pip install --quiet --upgrade pip cffi
}

# ── enrol, if asked ──────────────────────────────────────────────────────────
CRED="$PWD/.seyd-py.key"
if [[ -n "${ENROL_TOKEN:-}" ]]; then
  echo "building seydd for the enrolment step…"
  cargo build -p seydd --release --quiet
  CFG=$(mktemp -t seydd-py-enrol.XXXXXX)
  printf '[agent]\nrobot_id = "%s"\nsignal_url = "%s"\ncredential_path = "%s"\n' \
    "$ROBOT_ID" "$SIGNAL_URL" "$CRED" > "$CFG"
  ./target/release/seydd --config "$CFG" enrol --token "$ENROL_TOKEN"
  rm -f "$CFG"
fi

# ── run ──────────────────────────────────────────────────────────────────────
pkill -f "examples/ffmpeg_robot.py" 2>/dev/null || true

PAGE="${SIGNAL_URL%/ws}"
PAGE="${PAGE/#wss:/https:}"
PAGE="${PAGE/#ws:/http:}"
echo
echo "robot $ROBOT_ID → $SIGNAL_URL (profile $PROFILE, video $DEVICE)"
echo "pilot  $PAGE/?robot=$ROBOT_ID&signal=$SIGNAL_URL"
echo

# --command-udp mirrors what seydd does with a command channel, so pilot input
# lands on :5004 and tools/seyd-smoke.py can assert it.
exec "$PY" sdks/python/examples/ffmpeg_robot.py \
  --robot-id "$ROBOT_ID" \
  --signal "$SIGNAL_URL" \
  --profile "$PROFILE" \
  --device "$DEVICE" \
  --credential "$CRED" \
  --command-udp 5004
