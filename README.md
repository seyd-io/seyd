# Seyd

Remote operation of robots over the internet: low-latency video, sensors and
commands between a machine and an operator's browser, over one direct,
encrypted QUIC connection. You keep the robot, the camera, the control
software and the operator UI; Seyd is the connection between them.

- **On the robot**: a small agent forwards the camera's already encoded
  frames (no transcoding, ever), sensor messages and operator commands. Run it
  as the `seydd` daemon next to any camera that publishes RTSP or RTP, or
  embed it with the Python, C or Rust SDK when your own process holds the
  frames.
- **In the browser**: `<seyd-video>` connects, decodes with WebCodecs and
  paints the feed, paced on the camera's own clock; `SeydSession` is the same
  engine with no UI for a page of your own. Chrome and Edge, desktop and
  Android.
- **In between**: a cloud that introduces the two and checks who may connect,
  then gets out of the way. Direct first; a metered relay only as the last
  resort, and always shown as such.

The developer documentation is at
**https://seyd-signal-flj7s44j4a-ew.a.run.app/docs/** (built from this
repository: `web/docs`). The hosted cloud, console and deploy tooling are a
separate private repository; the protocol they speak is documented here.

## Try it in five minutes

The public demo robot is a real steerable camera. Open
<https://seyd-signal-flj7s44j4a-ew.a.run.app/pilot/?robot=seyd-demo> in
Chrome, press `S` for the HUD, drag to pan. Then put it in a page of your
own: [Quickstart](https://seyd-signal-flj7s44j4a-ew.a.run.app/docs/start/quickstart/).

## Build from source

Nothing is published to npm, PyPI or crates.io yet. You need Rust (rustup),
Node 22 with pnpm, and Python 3.

```bash
cargo build -p seydd --release        # the daemon → target/release/seydd
cargo build -p seyd-ffi --release     # libseyd for the Python and C SDKs
pnpm install && pnpm -r build         # @seyd/core, @seyd/web, the pilot page, the docs
```

`tools/setup-machine.sh` does all of it on a fresh Mac. Then:

| You have | Start at |
|---|---|
| A camera publishing RTSP or RTP | [Integrate the daemon](https://seyd-signal-flj7s44j4a-ew.a.run.app/docs/robot/daemon/) |
| Encoded frames in your own process | [Choose a form factor](https://seyd-signal-flj7s44j4a-ew.a.run.app/docs/start/form-factor/) |
| A web app that needs the feed | [The web components](https://seyd-signal-flj7s44j4a-ew.a.run.app/docs/pilot/web-components/) |
| A coding agent | [Integrate with a coding agent](https://seyd-signal-flj7s44j4a-ew.a.run.app/docs/start/agent-skill/): the skill in `skills/seyd/` |

## Repository

```
packages/   the Rust core: wire format, FEC, QoS, NAT traversal, transport, signalling, the agent, the C ABI, seydd
sdks/       C (generated seyd.h), Python (cffi over libseyd), @seyd/core and @seyd/web
web/        the design system, the landing and pilot pages, the developer docs
examples/   the demo robots: a Hikvision PTZ camera, a Tello drone
skills/     the integration skill for coding agents
docs/       architecture decision records, protocol contracts, encoder setup, latency
tools/      harnesses and the docs generators
sim/        a simulated robot (webcam → RTP) for development
```

`CLAUDE.md` is the engineering guide: the rules every change follows,
including the one that no public surface changes without its documentation.
`PLAN.md` is the plan; `docs/adr/` records every decision of weight.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md): the verification set, the DCO, and
the documentation rules. Security reports: [SECURITY.md](SECURITY.md).

## License

Apache-2.0. Copyright 2026 Anton Gravestam. See [LICENSE](LICENSE) and
[NOTICE](NOTICE).
