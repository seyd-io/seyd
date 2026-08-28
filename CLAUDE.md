# DARC — Monorepo Guide for Claude

## What this repo is

This is the monorepo for DARC (Distributed Autonomous Remote Control), a SaaS connectivity platform for remote operation of autonomous vehicles and robots. DARC provides low-latency, peer-to-peer video and data tunneling between robot systems and human operators over the public internet.

The repo will grow to include the core product, SDKs, a signaling server, a web-based pilot application, landing pages, marketing assets, tooling, and infrastructure configuration. Everything lives here.

## Key documents — read these first

Before writing any code or making any architectural decision, read:

- **SPEC.md** — the product specification: what DARC is, the two customer archetypes, the no-transcoding principle, technology choices, competitor landscape, and open questions.
- **PROTOTYPE.md** — the current build target: a Mac-to-Mac teleoperation demo using FFmpeg, a Python relay agent, a browser pilot page, and a WebSocket signaling server.
- **DEMO.md** — the always-on public demo: a real Hikvision PTZ camera over RTSP with operator pan/tilt/zoom. The first configuration where a real camera, not `sim/`, publishes the video.

These documents are the source of truth for product decisions. They are not static — they must be updated whenever we make a decision, change direction, or learn something new.

## Keeping documentation current — mandatory

When you make a decision during implementation that isn't reflected in SPEC.md or PROTOTYPE.md, update the relevant document before moving on. This applies to:

- Technology choices (e.g. "we chose library X over Y and here's why")
- Interface changes (e.g. "the signaling message format changed")
- Scope changes (e.g. "we added a feature" or "we cut something")
- Discoveries (e.g. "STUN failed in this scenario, so we added TURN")
- Architectural pivots (e.g. "we moved from Python to Go for the agent")

The rule: **if a future developer reading only SPEC.md and PROTOTYPE.md would be surprised by the code, the docs are out of date.** Fix the docs.

## Monorepo structure

```
darc/
├── CLAUDE.md              # this file
├── SPEC.md                # product specification
├── PROTOTYPE.md           # prototype build spec
│
├── packages/              # core DARC product components
│   ├── signal/            # darc-signal: WebSocket signaling server (Node.js)
│   ├── agent/             # darc-agent: robot-side relay daemon (Python)
│   └── pilot/             # darc-pilot: browser operator application (HTML/JS)
│
├── sim/                   # robot simulation — NOT part of DARC
│   ├── video-source.sh    # FFmpeg webcam → RTP/H.264 UDP :5000 (owns encoder settings)
│   └── sensor-source.py   # counter → UDP :5002 at 10 Hz
│
├── deploy/                # cloud infrastructure and deployment config
│
├── tools/                 # internal tooling and test harnesses
│   ├── setup-machine.sh   # dev machine bootstrap
│   ├── find-camera.py     # locate an IP camera on the LAN (SADP/ONVIF/port scan)
│   ├── relay-pilot.py     # headless pilot exercising the relay path
│   ├── pilot-smoke.py     # drives the real pilot in Chrome; asserts P2P + decode
│   ├── fec-vectors.py     # emit FEC interop vectors from the agent encoder
│   └── fec-check.js       # replay them through the pilot decoder
│
├── web/                   # (future) landing page and marketing site
└── docs/                  # (future) developer documentation and SDK guides
```

**Import direction:** `tools/` may reach into `packages/`. `packages/` must never
reach into `tools/` or `sim/`.

## Where QoS settings live

Encoder settings (resolution, preset, VBV, GOP) belong to the robot's video
publisher — `sim/video-source.sh` in the prototype. DARC states only
transport-observable *targets* (`packages/agent/qos.py`: bitrate ceiling, latency
budget, max GOP) plus its own transport policy (FEC rate, drop threshold). If you
find yourself putting a resolution in `qos.py`, or an FEC percentage in `sim/`,
the boundary has leaked.

## Component philosophy

**Loose coupling.** Components communicate only through defined interfaces: UDP sockets and WebSocket messages. No component imports or calls into another's internals.

**High cohesion.** Each component does one thing. Do not add responsibilities to a component because it is convenient — create a new component or a well-defined interface.

**Production-boundary awareness.** Label code clearly: is this a production DARC component, or a prototype stand-in? The `sim/` directory exists precisely to keep fake robot code out of real DARC components. Never import from `sim/` in `packages/`.

**One known exception: `packages/agent/camera.py`.** It is a Hikvision ISAPI
driver living inside a DARC component, which is against the grain of everything
above. Elsewhere the agent states intent and lets the robot decide how to meet it
— `PublisherControl` fires a JSON target at a UDP port and does not implement the
publisher. A production DARC should do the same for actuation: forward a generic
`ptz` intent over a robot-control interface and let robot-side code speak ISAPI,
ONVIF, or ROS2. It is where it is because DEMO.md specifies it there and the demo
needed one concrete camera to work. Nothing above `CameraControl` knows the word
"Hikvision"; keep it that way, and move it out rather than adding a second vendor
beside it.

**No transcoding in DARC.** The DARC Agent is a pure relay. It forwards bytes. It does not decode, re-encode, or inspect media payloads.

## Technology choices (current)

| Component | Language / runtime | Rationale |
|---|---|---|
| darc-signal | Node.js | Fast iteration, good WebSocket support, easy cloud deploy |
| darc-agent | Python + PyAV (av) + aioquic + websockets | PyAV demuxes H.264 from RTP; forwarded byte-for-byte. NAT traversal (STUN, PCP/NAT-PMP/UPnP) and Reed-Solomon FEC implemented directly — no miniupnpc, no numpy — to stay on pure-Python wheels for the ARM cross-compile |
| darc-pilot | Vanilla HTML/JS + WebCodecs | WebCodecs VideoDecoder eliminates jitter buffer; canvas render, no `<video>` element |
| Loss resilience | Reed-Solomon over GF(256), Cauchy matrix | FEC not retransmission: a retransmit costs a round trip, which teleoperation cannot spend. Paired implementations in `packages/agent/fec.py` and `packages/pilot/fec.js` — they must agree byte-for-byte, enforced by `tools/fec-check.js` |
| Video source (sim) | FFmpeg | Standard RTP/H.264 output; matches what real robot camera nodes produce. Bitrate is **capped** — CRF mode produced unbounded 8–14 Mbps spikes on motion |

**Video path (prototype):** FFmpeg H.264 encode → RTP UDP → PyAV demux → binary WebSocket → signal relay → WebCodecs VideoDecoder → canvas. Single encode chain. No transcoding, no jitter buffer.

**Video path (production):** FFmpeg H.264 encode → QUIC datagrams (MsQuic) → WebTransport → WebCodecs VideoDecoder → canvas. P2P, no relay.

Technology choices may change. When they do, update this table and document the reason in SPEC.md.

## Hosting (current)

| Component | Platform | Notes |
|---|---|---|
| darc-signal | Google Cloud Run | WSS out of the box, scales to zero, no server management |

**Cloud Run specifics:**
- Container images stored in Google Artifact Registry (`europe-west1`)
- `--max-instances 1` for prototype (in-memory session state; revisit when sessions move to a shared store)
- `--allow-unauthenticated` for prototype; production adds token auth
- 30s WebSocket pings prevent Cloud Run's 60-minute idle timeout from closing active sessions

**Portability:** The server is a plain Docker container. Moving to another platform means pushing the same image to a different registry and updating the `--signal-url` config in agent and pilot. No application code changes.

Deployment instructions and one-time GCP setup: `packages/signal/DEPLOY.md`.

## What we do not build

- The robot hardware or vehicle
- The camera or sensor hardware
- The operator control algorithms or autopilot
- A managed human operator network
- A transcoding or media processing layer

## Current build target

See PROTOTYPE.md. The prototype is a Mac-to-Mac demo: one Mac on an iPhone 5G hotspot acts as the robot, one Mac on broadband acts as the pilot. Video is relayed through the signaling server (not P2P) — this is an explicit POC compromise. P2P via WebTransport is the phase 2 target.
