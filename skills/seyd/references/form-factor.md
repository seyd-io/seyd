# Choosing a form factor

Every form factor is the same Rust core (`seyd_core::Agent`) with a different
host around it: the wire format, FEC, NAT traversal, QoS, signalling and the
agent lifecycle are in the core, so the hosts cannot drift apart. The
question that picks one is where the encoded frames are.

| Form factor | Your frames are | You write | Status |
|---|---|---|---|
| **The daemon, `seydd`** | On a socket: the camera or pipeline publishes RTSP or RTP | A TOML file; small programs on its UDP interfaces. No Seyd code | Built |
| **Python SDK, `seyd`** | In your own Python process, as access units | `Agent`, `add_channel`, `push_frame`, a few handlers | Built; cffi over `libseyd`, reads its declarations from `seyd.h` |
| **C ABI, `seyd.h`** | In your own process, any language with a C FFI | Against the header, or a wrapper | Built, ABI version 1; C++ and ROS 2 wrappers planned, not built |
| **Rust, `seyd_core::Agent`** | In your own Rust program | A host of your own, as `seydd` and `seyd-ffi` are hosts | Built |

## When each is right

**`seydd`** when the camera already publishes RTP or RTSP. The daemon owns
depacketization, reads UDP for sensors, writes commands to UDP, and posts
publisher control (bitrate targets, keyframe requests, session events) as
JSON datagrams. Enrolment is built in. An install is one binary, one file and
a systemd unit. Most IP cameras and most existing robot stacks land here,
including stacks whose own software is in C++ or ROS 2: they talk to the
daemon over UDP and never link anything. See `robot-daemon.md`.

**Python SDK** when the Python process already holds encoded frames and a
socket would be a copy and a hop for nothing: an encoder you drive, a
GStreamer or FFmpeg pipeline you own, a research stack. See `robot-sdk.md`.

**C ABI** when the language is not Python and the frames are in-process. The
header is cbindgen output from the Rust crate and is checked in, so a
consumer needs no Rust toolchain once `libseyd` is built. Additions are
appended, never reordered, until the version is bumped.

**Rust crate** when the robot program is Rust. The host supplies media and
consumes `AgentEvent`; it owns nothing of the lifecycle.

RTP and RTSP ingest is the daemon's job, not the ABI's. A robot whose camera
publishes on a socket should run `seydd` rather than write depacketization
against the header.

## Gaps to put in the plan

- **No packages are published** (npm, PyPI, crates.io). Everything builds
  from a checkout of the Seyd repository: `cargo build -p seydd --release`
  (the daemon), `cargo build -p seyd-ffi --release` (`libseyd` for Python and
  C), `pnpm install && pnpm -r build` (`@seyd/core`, `@seyd/web`). The
  toolchain is Rust (rustup), Node 22 with pnpm, Python 3 with `cffi` for
  the Python SDK.
- **The C ABI has no enrolment call**, so a Python or C robot cannot redeem
  an enrolment token itself. Enrol its key file with `seydd enrol --token …`
  against the same `credential_path` (same file format), then start the SDK
  robot. Planned as an append to the ABI.
- **The Python SDK does not expose simulcast layers**; the daemon, the C ABI
  (`seyd_channel_add_layer`, `seyd_push_frame_layer`) and Rust
  (`ChannelSpec.layers`, `push_video_layer`) do.
- **`max_bitrate_kbps`, `relay` and `recovery_ladder`** are settable in
  `seydd.toml` and `AgentConfig`/`ChannelSpec` (Rust), not through the C ABI
  or Python yet; those take the defaults (no publisher limit, relay allowed,
  ladder on).
- **The daemon's video inputs are `rtsp://` and `rtp://` only**, with codecs
  H.264 (`avc1.…`, RTSP and RTP), H.265 (`hev1.…`/`hvc1.…`, RTSP and RTP) and
  MJPEG (`mjpeg`, RTSP only; the daemon refuses `rtp://` MJPEG). Sensor input
  is `udp://`; command output is `udp://`.
- **The pilot is Chrome or Edge**, desktop or Android. No Safari, so nothing
  on iPhone or iPad. There is no native pilot SDK; mobile SDKs come when a
  customer needs them.
- **One signal server per robot and pilot.** Both must use the same
  `signal_url`; that is where the introduction happens.

## What every form factor shares

- Lifecycle: create the agent, add channels, start, serve sessions, stop.
  Channels are added before start, never after, because the channel list is
  announced to the cloud and read by the pilot from the first message of
  every session. Channels are numbered from 1 in declaration order.
- Seyd is handed encoded bytes and relays them; it never decodes,
  re-encodes or inspects payloads.
- Callbacks run on one Seyd thread and must not block.
- Pushing a frame or a message never blocks; a frame that arrives while the
  previous one is still going out is dropped whole by admission control and
  counted (`frames_dropped_backlog`); keyframes never.
- Pass the encoder's own capture timestamp.
- Answer `video-config` and `recovery-request` (`publisher-contract.md`).
- Park actuators when a session ends.
