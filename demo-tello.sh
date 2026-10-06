#!/usr/bin/env bash
# Start the Tello drone demo robot (DEMO-TELLO.md) on the Seyd stack: seydd
# (generic daemon) + examples/tello-robot/bridge.py (the drone-specific part).
#
#   ./demo-tello.sh                                    # drone on Wi-Fi, deployed cloud
#   ./demo-tello.sh --fake                             # no drone: fake_tello.py on localhost
#   ./demo-tello.sh --rust                             # the native Rust host instead of bridge.py + seydd
#   SIGNAL_URL=ws://localhost:8080/ws ./demo-tello.sh  # against a local cloud/api
#   BRIDGE_ARGS=--no-takeoff ./demo-tello.sh           # bench: props off, take-off refused
#   ENROL_TOKEN=seyd_enr_… ./demo-tello.sh             # first start on a new key
#
# The laptop joins the drone's Wi-Fi (TELLO-xxxxxx, 192.168.10.0/24) and keeps
# the internet on Ethernet. The preflight checks exactly that: the Wi-Fi
# interface holds a 192.168.10.x address, the default route does NOT go over
# it, and the drone answers on 8889. Overrides: TELLO_IP, ROBOT_ID,
# DARC_QOS_PROFILE, TELLO_ALT_LIMIT_M, RUST_LOG (see examples/tello-robot/run.sh).
set -euo pipefail
cd "$(dirname "$0")"
if [ -f .env.local ]; then set -a; . ./.env.local; set +a; fi

FAKE=0
for arg in "$@"; do
  case "$arg" in
    --fake) FAKE=1 ;;
    --rust) export HOST=rust ;;
    -h|--help) sed -n '2,17p' "$0"; exit 0 ;;
    *) echo "unknown argument: $arg" >&2; exit 2 ;;
  esac
done

if [ "$FAKE" = 1 ]; then
  export TELLO_IP=127.0.0.1
  pkill -f "tello-robot/fake_tello.py" 2>/dev/null || true
  python3 examples/tello-robot/fake_tello.py ${FAKE_ARGS:-} &
  FAKE_PID=$!
  trap 'kill $FAKE_PID 2>/dev/null; kill 0 2>/dev/null' EXIT
  sleep 0.5
else
  TELLO_IP="${TELLO_IP:-192.168.10.1}"
  # ── network preflight (macOS): Wi-Fi on the drone, internet elsewhere ──
  WIFI_DEV=$(networksetup -listallhardwareports 2>/dev/null | awk '/Hardware Port: Wi-Fi/{getline; print $2}' | head -1)
  if [ -n "$WIFI_DEV" ]; then
    WIFI_IP=$(ipconfig getifaddr "$WIFI_DEV" 2>/dev/null || true)
    SSID=$(ipconfig getsummary "$WIFI_DEV" 2>/dev/null | awk -F': ' '/ SSID/{print $2; exit}' || true)
    case "$WIFI_IP" in
      192.168.10.*) echo "wi-fi $WIFI_DEV: $WIFI_IP on ${SSID:-drone network}" ;;
      *) echo "wi-fi $WIFI_DEV: ${WIFI_IP:-no address} (${SSID:-not joined}) — join the drone's TELLO-xxxxxx network first"; exit 1 ;;
    esac
    DEFAULT_IF=$(route -n get default 2>/dev/null | awk '/interface:/{print $2}' || true)
    if [ "$DEFAULT_IF" = "$WIFI_DEV" ]; then
      echo "default route goes over the drone's Wi-Fi ($WIFI_DEV) — the cloud is unreachable that way."
      echo "Plug in Ethernet and put it above Wi-Fi in System Settings → Network → ⋯ → Set Service Order."
      exit 1
    fi
    echo "internet via ${DEFAULT_IF:-?}"
  fi
  if ! ping -c 1 -W 1000 "$TELLO_IP" >/dev/null 2>&1; then
    echo "drone $TELLO_IP: no ping reply — is it powered on and is this laptop on its Wi-Fi?"; exit 1
  fi
  echo "drone $TELLO_IP: ok"
fi

if [ "$FAKE" = 1 ]; then
  examples/tello-robot/run.sh     # not exec: the trap above must outlive it to stop the fake
else
  exec examples/tello-robot/run.sh
fi
