# darc-agent — Setup

## System dependencies

aiortc uses PyAV (libav) for media. Install FFmpeg via Homebrew before installing Python packages:

```bash
brew install ffmpeg
```

## Python setup

```bash
cd packages/agent
python3 -m venv .venv
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
| `--sensor-port` | 5001 | UDP port the sensor source sends to |

## What the agent logs

```
12:34:01 INFO     registered robot_id=mac-robot-01
12:34:15 INFO     pilot connected — initiating WebRTC offer
12:34:15 INFO     video track attached from UDP RTP :5000
12:34:16 INFO     offer ready (ICE gathering complete)
12:34:16 INFO     remote description set (answer accepted)
12:34:16 INFO     connection state: connected
12:34:16 INFO     data channel open — binding sensor UDP :5001
12:34:45 INFO     [cmd] type=snapshot ts=1723456789123
```

## Notes

- The agent waits for ICE gathering to complete before sending the offer. This embeds all ICE candidates in the SDP (no trickle ICE for the offer). The pilot may still trickle ICE candidates back; the agent handles them.
- Video and sensor sources are started independently. The agent does not crash if they are not running — it simply has no data to relay until they start.
- This prototype version handles one session at a time. Connecting a second pilot while one is active is undefined behaviour.
