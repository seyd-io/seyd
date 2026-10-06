---
name: seyd
description: Integrate a robot, vehicle, drone or camera with Seyd, the connectivity layer for remote operation (low-latency video, sensors and commands between a machine and an operator's browser over one direct QUIC connection). Covers choosing the form factor (the seydd daemon, or the Python, C or Rust SDK), declaring channels, meeting the video publisher contract, enrolment and access, the pilot page (<seyd-video> or SeydSession), network reachability and verification. Use when the user mentions Seyd, seydd, libseyd, @seyd/web, @seyd/core, or wants to teleoperate / remotely drive / watch a robot or camera through Seyd. Interview the user for what the plan needs before writing code.
---

# Integrating with Seyd

Seyd puts a person at the controls of a machine anywhere on the internet. A
small program on the robot (the **agent**) forwards the camera's already
encoded video, sensor messages and operator commands over one direct,
encrypted QUIC connection to a web page in the operator's browser (the
**pilot**). The **Seyd cloud** only introduces the two and checks who may
connect; on a direct session it never sees a byte of video.

You keep the robot, the camera, the control software and the operator UI.
Seyd is the connection between them. An integration is therefore never
"install Seyd": it is a handful of decisions and a few pieces of code on both
ends, and this skill exists to make those decisions correctly and to leave
nothing out.

**Seyd does not** encode, decode or transcode video, run control loops, host
operators, or carry bulk transfer (logs, maps, software updates). If a plan
needs any of those, that part is the user's system, not Seyd.

## The parts of every integration

Every integration, whatever the form factor, has these parts. The plan you
produce must say what happens for each one; "not needed" is an answer, "not
mentioned" is not.

| Part | What it is | Reference |
|---|---|---|
| 1. The agent | The Seyd program on the robot: `seydd` (a daemon and a TOML file) or an SDK call in the user's own process | `references/form-factor.md` |
| 2. Video channels | Where the encoded frames come from and the codec string the pilot configures its decoder from | `references/robot-daemon.md`, `references/robot-sdk.md` |
| 3. The publisher contract | How the user's encoder answers Seyd's bitrate ceiling, GOP bound and keyframe requests | `references/publisher-contract.md` |
| 4. Sensor channels | Robot-to-pilot messages: telemetry, state, anything small and frequent | same as 2 |
| 5. Command channels and safety | Pilot-to-robot messages, who may send them, and what the robot does when the driver vanishes | same as 2, plus the safety rules below |
| 6. Enrolment and access | The robot's key, the one-time enrolment token, who may observe or drive, how a pilot gets a session token | `references/access.md` |
| 7. The pilot page | `<seyd-video>` dropped into a page, or a UI of the user's own on `SeydSession` | `references/pilot.md` |
| 8. The network | Whether an inbound UDP packet can reach the robot's QUIC port, and what to change if not | `references/networking.md` |
| 9. Verification | The ladder of checks from "the camera URL works" to "a pilot drove it through loss" | `references/verification.md` |

## Step 1: read before asking

Look at the user's codebase and environment before interviewing. Much of the
interview answers itself. Search for:

- **Video source**: `rtsp://`, `rtp://`, `gst-launch`, `appsink`, `ffmpeg`,
  `x264`, `nvenc`, `v4l2`, `libcamera`, `VideoEncoder`, `h264`, `hevc`, camera
  vendor names (Hikvision, Axis, ONVIF). An RTSP or RTP URL means the daemon;
  an encoder inside a process means an SDK.
- **Language and platform**: `CMakeLists.txt`, `package.xml` (ROS 2),
  `pyproject.toml`, `Cargo.toml`, `go.mod`, Dockerfiles, systemd units, a
  Jetson or Raspberry Pi mention, the target architecture.
- **Existing control and telemetry paths**: UDP sockets, ROS topics, MQTT,
  WebSockets, a JSON schema for commands or state. These become command and
  sensor channels, usually without changing their shape.
- **Operator side**: an existing web app and its framework, or none.
- **Seyd already present**: a `seydd.toml`, `import seyd`, `#include "seyd.h"`,
  `@seyd/web` in a `package.json`. Then this is a change, not a first
  integration; read `references/verification.md` first.

## Step 2: interview for what is missing

Ask only what the codebase did not answer, and ask the questions in one or
two batches, not one at a time. The full question bank, with why each
question matters and which decision it feeds, is in
`references/interview.md`. The decisions the answers must settle:

1. **Where the encoded frames are.** On a socket (RTSP/RTP from a camera or a
   pipeline) or inside the user's process. This alone picks the form factor.
   If there is no encoder yet, the plan includes choosing and configuring one
   (`references/publisher-contract.md`, "Encoders").
2. **What the encoder can do at run time.** Bitrate cap, GOP length, intra
   refresh, a keyframe on request, a second lower-resolution stream. This
   decides how the publisher contract is met and whether simulcast applies.
3. **The language and the platform** the robot program runs on, and whether
   a daemon with a systemd unit is acceptable.
4. **Sensors and commands**: what they carry, how often, which process holds
   them, and what "safe" means when the driver disappears (stop, park, hover,
   land, return to home).
5. **Pilots**: who they are, which browser (Chrome or Edge only; nothing on
   iOS), whether the user's own web app hosts the feed, and how pilots are
   authenticated (public, Seyd console users, or the user's own backend
   minting session tokens with an API key).
6. **The network** between the robot and the internet: home or office router,
   corporate NAT, a 4G/5G SIM (almost always CGNAT), Starlink, a static
   public address. Whether a UDP port can be forwarded. Whether a metered
   relay is acceptable as the last resort.
7. **The cloud**: the hosted Seyd cloud or a self-hosted one; whether the user
   has an organisation and can mint enrolment tokens; data residency needs.
8. **The bar**: latency target, uplink bandwidth, number of simultaneous
   pilots, and what the user will accept as "done".

Stop interviewing when you can fill every section of the plan template. Do
not ask about things the plan does not need.

## Step 3: choose the form factor

Where the encoded frames are decides it (`references/form-factor.md` has the
full table and the gaps of each):

| Frames are | Use | The user writes |
|---|---|---|
| On a socket (camera or pipeline publishes RTSP or RTP) | **`seydd`**, the daemon | A TOML file and small programs on its UDP interfaces; no Seyd code |
| In the user's Python process | **Python SDK** (`seyd`, cffi over `libseyd`) | `Agent`, `add_channel`, `push_frame`, handlers |
| In a process in C, C++, Go or any language with a C FFI | **C ABI** (`seyd.h`, `libseyd`) | Against the header, or a thin wrapper |
| In the user's Rust program | **Rust crate** (`seyd_core::Agent`) | A host: supply media, consume `AgentEvent` |

Prefer the daemon whenever it fits: it is the least code, it already does
RTP/RTSP depacketization, and it has built-in enrolment. Do not write
depacketization against the SDK for a camera that already publishes RTSP.
Raw frames with no encoder are not a form-factor question until an encoder
exists; a GStreamer or FFmpeg pipeline that publishes RTP to the daemon is
usually the shortest path.

## Step 4: write the plan, then show it

Produce an integration plan before writing code, in this shape, and get the
user's agreement on it. The template with guidance per section is at the end
of `references/interview.md`.

1. **Summary**: the robot, the operator, what the session carries.
2. **Form factor and why**, including what that form factor cannot do today
   (`references/form-factor.md`, "Gaps").
3. **Channels**: a table of every channel, numbered from 1 in declaration
   order, with kind, name, codec string, source or sink, rate.
4. **Video publisher**: the encoder, its settings against the contract (no
   B-frames, parameter sets inline, cap at or under `maxBitrateKbps`, VBV
   about 100 ms, intra refresh or a long GOP, keyframe on request), and how
   each `video-config`, `recovery-request` and `layer` message is answered.
   Simulcast if a second stream exists.
5. **Robot-side program**: what is written where, the safety behaviour on
   session end, and the stale-command rule.
6. **Enrolment and access**: org, enrolment token, the credential file and
   its backup, grants, how the pilot gets a session token.
7. **Pilot page**: `<seyd-video>` or `SeydSession`, the controls, the token
   fetch, the HUD and the guidance box.
8. **Network**: the expected candidates for this network, the forward or
   pinhole to make, whether the relay is allowed.
9. **Verification**: the steps from `references/verification.md` that apply,
   with what "good" looks like for each.
10. **Open questions** and **out of scope**.

## Step 5: implement

Work through the plan in this order, because each step is checkable before
the next and the later ones depend on the earlier ones: the video input
alone (`seydd --probe-input`, or frames reaching `push_frame`), then the
agent announced to the cloud, then a pilot connected on the LAN, then the
publisher contract answered, then commands and sensors, then access tightened
(no public grant), then the network beyond the LAN, then loss.

The rules that hold in every form factor; a plan that breaks one is wrong:

- **Hand Seyd whole encoded access units** and nothing else. It never decodes,
  re-encodes or inspects a payload. One `push_frame` per picture, with the
  keyframe flag the encoder gave you. Never transcode to fit.
- **Pass the encoder's own capture timestamp** (`capture_ts_us`). The pilot
  paces presentation on it; passing "now" adds jitter the pilot cannot
  remove.
- **The codec string is a WebCodecs identifier** (`avc1.42001f`,
  `avc1.420028` for 1080p, `hev1.…`, `mjpeg`) and it reaches the browser's
  decoder unchanged, so the profile and level must admit the real picture.
- **Callbacks and event handlers run on one Seyd thread and must not block.**
  Queue the work and return. `push_frame` and `push_message` never block
  either; a frame that arrives while the previous one is still going out is
  dropped whole and counted, keyframes never.
- **Answer the publisher requests.** `video-config` (bitrate ceiling, latency
  budget, GOP bound, intra-refresh preference, frame-rate hint) and
  `recovery-request` (`ltr`, `intra_refresh` or `idr`). Keyframes are on
  demand: a periodic one-second IDR is forbidden by design (it is the
  latency tail and a large share of the bandwidth); `maxGopMs` is a safety
  net of 10 s (4 s on `quality`), not a cadence. An integration that cannot
  answer `recovery-request` leaves a joining pilot waiting up to `maxGopMs`
  for a first picture.
- **Degrade resolution first, frame rate last.** For a remote pilot the frame
  interval is latency. Resolution, preset and VBV are the publisher's to
  choose; a Seyd profile never contains a resolution.
- **Park actuators when the session ends.** The event fires whether the
  pilot said goodbye or simply vanished. Drop stale commands: a command that
  sat in a queue is not the operator's current intent.
- **Observers cannot command.** The first pilot is the driver; later ones are
  observers and the robot itself drops their commands. Do not build a second
  permission scheme for this.
- **Direct first; the relay last and always shown.** Never configure a system
  to prefer the relay and never hide a relayed session from the operator.
  It is metered, adds a hop, and is the symptom of a network problem whose
  fix is in `references/networking.md`.
- **Secrets stay out of files where possible**: `SEYD_ENROLMENT_TOKEN`,
  `SEYD_RTSP_USER`, `SEYD_RTSP_PASSWORD` in the environment; the robot's key
  file (`credential_path`) is what stays enrolled, so it is backed up and
  never shared between robots.

## Step 6: verify

Never declare an integration done on "it connected once". Walk the ladder in
`references/verification.md`: input probe, log lines, first picture time,
HUD numbers, a keyframe-request probe, a command round trip, session-end
safety, then loss injection (`?loss=0.05`) and, for a field deployment, the
race from a network that is not the robot's LAN. Report measured numbers,
not impressions.

## Where the truth is

This skill summarises the developer documentation and the code; when it and
the code disagree, the code wins. Two files in `references/` are generated
from the code and are exact: `seydd-config.md` (every key of `seydd.toml`)
and `qos-profiles.md` (every number of the three QoS profiles). Routes
written as `/docs/…` are pages of the developer documentation, served at
`<signal server origin>/docs/` (today
`https://seyd-signal-flj7s44j4a-ew.a.run.app/docs/`); the hosted signal
server is `wss://seyd-signal-flj7s44j4a-ew.a.run.app/ws`.

Nothing is published to npm, PyPI or crates.io yet. Every form factor is
built from a checkout of the Seyd repository, `https://github.com/seyd-io/seyd` (`cargo build -p seydd
--release`, `cargo build -p seyd-ffi --release`, `pnpm install && pnpm -r
build`); `references/form-factor.md` says what each one needs. The
developer therefore has a checkout, and you should read these files in it
when the plan touches their subject rather than work from this summary
alone:

| Need | File |
|---|---|
| A real daemon config with simulcast | `examples/demo-robot/seydd.toml` |
| A real publisher-control consumer (camera driver, PTZ, keyframe requests) | `examples/demo-robot/bridge.py` |
| A complete Python robot with its own x264 encoder | `sdks/python/examples/ffmpeg_robot.py` |
| A complete C robot (sensor + command) | `sdks/c/examples/sensor-robot.c` |
| The C header, generated and checked in | `sdks/c/include/seyd.h` |
| A complete Rust host | `packages/seyd-core/examples/minimal_host.rs` |
| The smallest pilot page | `sdks/js/web/examples/minimal.html` |
| A pilot with no UI on `SeydSession` | `sdks/js/core/examples/headless-session.ts` |
| The public demo pilot page (controls, token fetch, role handling) | `web/demo/src/main.ts`, `web/demo/src/ptz.ts` |
| A drone integration through an unchanged daemon (safety rules, stick hold, orphan landing) | `examples/tello-robot/bridge.py`, `DEMO-TELLO.md` |
| Per-encoder recipes (x264, GStreamer, NVENC, Jetson, Pi, Hikvision, Axis, ONVIF) | `docs/encoder-setup.md` |
| The daemon's protocol contract (publisher-control messages) | `docs/protocol/seydd.md` |
| The decisions and their measurements | `docs/adr/` |
