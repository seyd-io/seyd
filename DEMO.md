# DARC Demo — Always-On Hikvision PTZ

## Purpose

A permanently-on demo robot that potential clients can discover on the fleet page, connect to, and control in real time. The goal is to let prospects experience the DARC feedback loop themselves — not watch a video of it — in a controlled environment with no moving parts to crash or break.

---

## Hardware

| Item | Model | Cost |
|---|---|---|
| PTZ camera | Hikvision DS-2DE2A404IWG-E (4MP, 4× optical) | ~$180 |
| PoE injector or switch | Any 802.3af injector | ~$25 |
| Desk / wall mount arm | Any ¼-20 ball head arm | ~$20 |
| Agent host | Mac mini or Raspberry Pi 5 | existing / ~$80 |
| **Total** | | **~$225–305** |

The camera is always on, pointed at something visually interesting in a controlled space (a shelf, a model, a small set). Clients pan and tilt to look around.

---

## Architecture

```
Office LAN
┌─────────────────────┐  RTSP/TCP (H.264)   ┌──────────────────────┐
│  Hikvision PTZ      │────────────────────► │  DARC Agent          │
│  192.168.x.x:554    │◄────────────────────│  (Mac mini / Pi 5)   │
│                     │  HTTP CGI (PTZ cmds) │                      │
└─────────────────────┘                      └──────────┬───────────┘
                                                        │ WSS
                                             ┌──────────▼───────────┐
                                             │  darc-signal          │
                                             │  (Cloud Run)          │
                                             └──────────┬───────────┘
                                                        │
                                              client browsers worldwide
```

**No FFmpeg intermediary.** PyAV (libavformat) opens the camera's RTSP stream directly. The H.264 bitstream demuxed from RTSP is identical to what PyAV currently demuxes from RTP/UDP — the downstream relay path (NAL extraction → binary WebSocket → WebCodecs) is unchanged.

---

## Agent changes required (not yet implemented)

### 1. Dual video input mode

Add `--video-url` as an alternative to `--video-port`. When `--video-url` is set, `peer.py` opens the RTSP URL directly via `av.open()` instead of reading from an SDP file:

```python
# current (UDP RTP via SDP file)
container = av.open(sdp_path, format='sdp', options={...})

# new (RTSP direct)
container = av.open('rtsp://admin:password@192.168.x.x:554/Streaming/Channels/101',
                    options={'rtsp_transport': 'tcp', 'fflags': 'nobuffer', ...})
```

Everything downstream of `container.demux()` is identical. No other changes to the relay path.

### 2. PTZ command handler

The agent needs a camera control adapter that:
1. Receives `{"type": "ptz", "pan": <-100…100>, "tilt": <-100…100>}` on the data channel
2. Calls the Hikvision HTTP CGI API:

```python
import requests

CAMERA_IP  = os.getenv('CAMERA_IP', '192.168.1.100')
CAMERA_AUTH = ('admin', os.getenv('CAMERA_PASSWORD', ''))

def ptz_move(pan: int, tilt: int):
    requests.put(
        f'http://{CAMERA_IP}/ISAPI/PTZCtrl/channels/1/continuous',
        json={'PTZData': {'pan': pan, 'tilt': tilt, 'zoom': 0}},
        auth=CAMERA_AUTH,
        timeout=0.5,
    )

def ptz_stop():
    ptz_move(0, 0)
```

Command handling in `peer.py`'s `handle_message()` dispatches on `type == 'ptz'` to these functions.

### 3. New CLI flags

```
--video-url    rtsp://... (alternative to --video-port for IP cameras)
--camera-ip    Hikvision camera IP for PTZ control
--camera-pass  Hikvision admin password (or via env CAMERA_PASSWORD)
```

---

## Pilot changes required (not yet implemented)

### PTZ joystick control

The pilot page needs a control surface for pan/tilt. Options:

- **Click-and-hold overlay arrows** (simplest): four arrow buttons on the video overlay, `mousedown` sends `{"type":"ptz","pan":50,"tilt":0}`, `mouseup` sends stop.
- **Mouse drag on canvas**: drag direction and distance map to pan/tilt speed.
- **Gamepad API**: map left stick to pan/tilt — best for actual feel, good for demo.

The stop command (`pan:0, tilt:0`) must be sent on `mouseup` / `touchend` / stick release, or the camera keeps moving.

---

## Demo environment

- Camera pointed at a curated, visually interesting scene (model, art, branded backdrop, window view)
- Pan range is physically limited by mount position — no need to guard against hitting stops
- Robot ID: `darc-demo` (always listed on fleet page)
- Latency notice shown on pilot page: "You are controlling a physical camera. Response latency reflects your connection."

---

## Open questions

1. **Multiple simultaneous pilots**: when two clients connect, the current signal server gives the robot to the last connected pilot. For the demo, this might be fine (last-writer-wins), or we may want queuing or read-only observer mode.
2. **PTZ presets**: should the camera auto-return to a home position after a client disconnects? Hikvision supports preset positions via the same HTTP API.
3. **Credentials management**: camera password should not be in the agent CLI args — use env var or a secrets file.
4. **Gamepad vs mouse control**: decide on the primary control surface before implementing the pilot UI changes.
5. **Demo branding**: should the pilot page show different copy ("You are controlling a real camera") vs the generic UI?
