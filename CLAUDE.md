# DARC — Monorepo Guide for Claude

## What this repo is

This is the monorepo for DARC (Distributed Autonomous Remote Control), a SaaS connectivity platform for remote operation of autonomous vehicles and robots. DARC provides low-latency, peer-to-peer video and data tunneling between robot systems and human operators over the public internet.

The repo will grow to include the core product, SDKs, a signaling server, a web-based pilot application, landing pages, marketing assets, tooling, and infrastructure configuration. Everything lives here.

## Key documents — read these first

Before writing any code or making any architectural decision, read:

- **SPEC.md** — the product specification: what DARC is, the two customer archetypes, the no-transcoding principle, technology choices, competitor landscape, and open questions.
- **PROTOTYPE.md** — the current build target: a Mac-to-Mac teleoperation demo using FFmpeg, a Python relay agent, a browser pilot page, and a WebSocket signaling server.

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
│   ├── video-source.sh    # FFmpeg webcam → RTP/H.264 UDP :5000
│   └── sensor-source.py   # counter → UDP :5001 at 10 Hz
│
├── deploy/                # cloud infrastructure and deployment config
│
├── web/                   # (future) landing page and marketing site
├── docs/                  # (future) developer documentation and SDK guides
└── tools/                 # (future) internal tooling, dashboards, scripts
```

## Component philosophy

**Loose coupling.** Components communicate only through defined interfaces: UDP sockets and WebSocket messages. No component imports or calls into another's internals.

**High cohesion.** Each component does one thing. Do not add responsibilities to a component because it is convenient — create a new component or a well-defined interface.

**Production-boundary awareness.** Label code clearly: is this a production DARC component, or a prototype stand-in? The `sim/` directory exists precisely to keep fake robot code out of real DARC components. Never import from `sim/` in `packages/`.

**No transcoding in DARC.** The DARC Agent is a pure relay. It forwards bytes. It does not decode, re-encode, or inspect media payloads.

## Technology choices (current)

| Component | Language / runtime | Rationale |
|---|---|---|
| darc-signal | Node.js | Fast iteration, good WebSocket support, easy cloud deploy |
| darc-agent | Python + PyAV (av) + websockets | PyAV demuxes H.264 from RTP; forwarded byte-for-byte as binary WebSocket frames |
| darc-pilot | Vanilla HTML/JS + WebCodecs | WebCodecs VideoDecoder eliminates jitter buffer; canvas render, no `<video>` element |
| Video source (sim) | FFmpeg | Standard RTP/H.264 output; matches what real robot camera nodes produce |

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
