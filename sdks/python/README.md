# Seyd for Python

Hyper-low-latency point-to-point video, sensor and command streaming between a
robot and a human operator, over the public internet.

This package is a thin wrapper over `libseyd` (ADR 0004): all protocol logic —
the wire format, FEC, NAT traversal, QoS, signaling — lives in the Rust core, so
this SDK, the `seydd` daemon and the C++/ROS 2 wrappers cannot drift apart.

## Install

From a source checkout, with `libseyd` built by the workspace:

```bash
cargo build -p seyd-ffi --release
pip install cffi
PYTHONPATH=sdks/python python3 -c "import seyd; print(seyd.ABI_VERSION)"
```

The SDK finds the library at `$SEYD_LIBRARY`, then inside the installed
package, then in the workspace's `target/release` and `target/debug` — so it
works straight from a checkout with no install step.

## Use

```python
from seyd import Agent, ChannelKind

with Agent("my-robot", "wss://signal.seyd.io/ws", qos_profile="latency") as agent:
    video = agent.add_channel(ChannelKind.VIDEO, "main", codec="avc1.42001f", fps=25)
    telemetry = agent.add_channel(ChannelKind.SENSOR, "telemetry")
    agent.add_channel(ChannelKind.COMMAND, "drive")

    agent.on_command = lambda channel, payload: motors.apply(payload)
    agent.on_session_ended = lambda sid, reason: motors.park()

    agent.start()
    for au in my_encoder:
        agent.push_frame(video, au.bytes, keyframe=au.is_idr, capture_ts_us=au.pts_us)
```

Seyd relays **encoded** bytes. It never decodes, re-encodes or inspects
payloads — hand it what your encoder produced.

## Rules that matter

- **Handlers run on one Seyd thread and must not block.** A slow handler delays
  every later event. Queue the work; do not do I/O in a handler.
- **`push_frame` and `push_message` never block.** A frame arriving while the
  previous one is still going out is dropped by admission control — keyframes
  never are — and counted in `agent.counters.frames_dropped_backlog`.
- **Pass the encoder's own `capture_ts_us`** when you have it. The pilot paces
  presentation on the source's capture clock (ADR 0005); passing "now" instead
  of the capture time adds jitter the pilot cannot remove.
- **Park actuators in `on_session_ended`.** It fires whether the pilot said
  goodbye or simply vanished.
- **`on_requested_config` is where Seyd talks to your encoder** — a bitrate
  ceiling, a latency budget, a GOP bound. It states targets; the publisher
  decides resolution, preset and VBV. Ignoring it means Seyd's rate control
  cannot act on a congested link.

## Examples

`examples/ffmpeg_robot.py` is a complete robot: x264 through a pipe, access
units pushed straight into Seyd, a sensor channel and a command channel. Run it
against a local cloud with

```bash
python3 sdks/python/examples/ffmpeg_robot.py --robot-id seyd-py \
    --signal ws://localhost:8080/ws --device lavfi
```

then open `http://localhost:8080/?robot=seyd-py&signal=ws://localhost:8080/ws`.

## When to use `seydd` instead

If the robot's camera publishes RTP or RTSP on a socket, run the `seydd` daemon
— it owns the depacketization and needs only a TOML file. This SDK is for
robots whose own process already holds encoded frames, where going through a
socket would be a copy and a hop for nothing.

## Tests

```bash
python3 -m pytest sdks/python/tests
```
