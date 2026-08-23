# darc-agent — Setup

For a fresh machine, prefer running `./tools/setup-machine.sh` from the repo
root — it installs everything below automatically (Homebrew, Node, Python
3.12, FFmpeg) and works on both Apple Silicon and Intel Macs. The manual
steps here are for reference or partial setups.

## System dependencies

PyAV (libav) needs FFmpeg's underlying libraries. Install FFmpeg via Homebrew
before installing Python packages:

```bash
brew install ffmpeg
```

## Python setup

The agent requires Python 3.12 specifically (matches `dev.sh`/`robot.sh`):

```bash
cd packages/agent
python3.12 -m venv .venv
source .venv/bin/activate
pip install -r requirements.txt
```

## Run

```bash
python agent.py \
  --robot-id mac-robot-01 \
  --signal-url wss://darc-signal-<hash>-ew.a.run.app
```

Start the video source and sensor source before connecting a pilot:

```bash
# Terminal 1 — video source (from repo root)
bash sim/video-source.sh

# Terminal 2 — sensor source
python sim/sensor-source.py

# Terminal 3 — agent
source packages/agent/.venv/bin/activate
python packages/agent/agent.py --robot-id mac-robot-01 --signal-url wss://...
```

## Options

| Flag | Default | Description |
|---|---|---|
| `--robot-id` | required | Unique identifier for this robot |
| `--signal-url` | required | WebSocket URL of darc-signal (wss://...) |
| `--video-port` | 5000 | UDP port the RTP video source sends to |
| `--sensor-port` | 5002 | UDP port the sensor source sends to |
| `--webtransport-port` | 4433 | UDP port for the WebTransport (QUIC) server |
| `--webtransport-host` | (STUN-discovered) | Override STUN and advertise this host directly |

See `PROTOTYPE.md` for the full transport architecture (WebTransport P2P
with WebSocket relay fallback) and the video chunk wire format.

## Notes

- Video and sensor sources are started independently. The agent does not crash if they are not running — it simply has no data to relay until they start.
- This prototype version handles one session at a time. Connecting a second pilot while one is active is undefined behaviour.
