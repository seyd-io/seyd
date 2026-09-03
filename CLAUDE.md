# Seyd — Monorepo Guide for Claude

## What this repo is

This is the monorepo for **Seyd** (formerly DARC), a SaaS connectivity platform
for remote operation of autonomous vehicles and robots: hyper-low-latency,
point-to-point video, sensor and command streaming between robot systems and
human operators over the public internet, delivered as SDKs plus a signaling
cloud. Domains: `seyd.io`, `seydio.com`.

The product is a Rust core with a C ABI, a TypeScript web pilot SDK, and a
portable cloud. The Python/JS proof of concept that preceded it has been
deleted (git history before 2026-08-28 has it; PROTOTYPE.md describes it);
its measurements remain valid. `PLAN.md` is the plan; follow it.

## Key documents — read these first

- **PLAN.md** — the approved product plan: fixed decisions, the technology
  upgrades that define "world-class", the target architecture, ordered work,
  and verification. Start here.
- **docs/adr/** — architecture decision records. ADR 0001 (wire protocol v2),
  0002 (quinn), 0003 (MoQ), 0004 (C ABI), 0005 (presentation pacing),
  0006 (loss measured on one clock). Add one for every decision of that
  weight; never change a wire format or public API without one.
- **SPEC.md** — product specification: customers, use cases, no-transcoding
  principle, competitor landscape. Written under the DARC name; the product
  decisions section at the top records what changed on 2026-08-28.
- **PROTOTYPE.md** — the proof-of-concept build, frozen as history. Its
  measurements (FEC, NAT reachability table, decoder gotchas) remain valid
  inputs; its component descriptions describe the legacy Python/JS code only.
- **DEMO.md** — the always-on Hikvision PTZ demo. The first customer program
  the new stack must run.
- **DEMO-ROVER.md** — the planned second demo: a remotely driven rover in a
  booked, attended setting; v2 adds a second camera and a two-pilot
  driver/spotter model. Hardware not yet ordered.

## Fixed product decisions (2026-08-28) — do not re-open without the owner

- **P2P only.** No relay fallback. On P2P failure the pilot shows a diagnosis
  and concrete network fixes. Relays are a future, separately priced tier.
- **Rust core + C ABI.** All protocol logic in Rust crates; every other agent
  form factor (C++, Python, ROS 2, daemon) is a thin wrapper with no protocol
  logic. See ADR 0004.
- **Web pilot SDK first.** Mobile SDKs when a customer needs them.
- **Portable cloud, EU residency likely, no Google lock-in.** Cloud Run in
  `europe-west1` today; plain containers on Postgres + Redis; no GCP-only SDKs
  or services in application code; auth behind an OIDC abstraction, provider
  not yet chosen.
- **No timeline/headcount planning.** Plans are ordered work.

## Keeping documentation current — mandatory

When you make a decision that isn't reflected in PLAN.md, the ADRs or SPEC.md,
update the relevant document before moving on: technology choices, interface
changes, scope changes, discoveries, architectural pivots. The rule: **if a
future developer reading only PLAN.md, the ADRs and SPEC.md would be surprised
by the code, the docs are out of date.** Every measured number quoted in a doc
must be re-measured when the code it describes changes.

## Monorepo structure

```
seyd/
├── CLAUDE.md  PLAN.md  SPEC.md  PROTOTYPE.md  DEMO.md
├── Cargo.toml                 # Rust workspace
├── packages/                  # Seyd core — Rust, production
│   ├── seyd-wire/             # wire protocol v2 headers + legacy v1 decode (ADR 0001)
│   ├── seyd-fec/              # Reed-Solomon GF(256), Cauchy; must pass tools/fec-vectors.py
│   ├── seyd-qos/              # QoS profiles + closed-loop ABR controller (pure)
│   ├── seyd-nat/              # STUN, NAT classification, PCP/NAT-PMP/UPnP, candidates, NatReport
│   ├── seyd-transport/        # quinn: WebTransport (h3) + native QUIC (seyd/2); block sender
│   ├── seyd-signal-client/    # WS client to the cloud, signal v2, Ed25519 robot auth
│   ├── seyd-core/             # engine: channels, sessions, callbacks — pure Rust API
│   ├── seyd-ffi/              # C ABI → sdks/c/include/seyd.h
│   └── seydd/                 # daemon: TOML config, RTP/RTSP/UDP inputs
├── sdks/                      # thin wrappers — NO protocol logic here, ever
│   ├── c/  cpp/  python/  ros2/
│   └── js/core  js/web  js/react      # @seyd/core, <seyd-video>, <SeydVideo/>
├── cloud/api  cloud/prober  cloud/monitor  cloud/db
├── web/site  web/console  web/demo
├── docs/                      # ADRs (docs/adr/) and, later, the developer docs site
├── deploy/                    # Terraform (GCP isolated to one module), Dockerfiles, compose
├── examples/demo-robot/       # the Hikvision PTZ demo as a customer program
├── sim/                       # robot simulation — NOT part of Seyd
│   ├── video-source.sh        # FFmpeg webcam → RTP/H.264 UDP :5000 (owns encoder settings)
│   └── sensor-source.py       # counter → UDP :5002 at 10 Hz
├── demo-seyd.sh               # start the camera demo robot
└── tools/                     # harnesses: seyd-smoke.py, cdp.py, fec-vectors.py + fec-reference/, find-camera.py, setup-machine.sh
```

**Import direction:** `tools/` may reach into `packages/`. `packages/` and
`sdks/` must never reach into `tools/` or `sim/`. `sdks/` may only call the
C ABI or `@seyd/core`; if a wrapper grows a parser or a heuristic, move it into
a crate.

## Where QoS settings live

Encoder settings (resolution, preset, VBV, GOP) belong to the robot's video
publisher — `sim/video-source.sh` in the simulation, the camera in the demo.
Seyd states only transport-observable *targets* (bitrate ceiling, latency
budget, max GOP) via `on_requested_config`, plus its own transport and pilot
policy (FEC rate, drop threshold, close-out deadlines, presentation delay). If
you find yourself putting a resolution in `seyd-qos`, or an FEC percentage in
`sim/`, the boundary has leaked.

## Component philosophy

**Loose coupling.** Components communicate only through defined interfaces:
the wire protocol, the signal protocol, the C ABI, UDP sockets.

**High cohesion.** Each crate does one thing. `seyd-wire` is layout only;
`seyd-fec` never parses a header; `seyd-qos` does no I/O.

**Production-boundary awareness.** `sim/` keeps fake robot code out of Seyd.
Vendor drivers (the Hikvision ISAPI code in `examples/demo-robot/hikvision.py`)
belong in `examples/`, not in a Seyd component — the agent states intent
(`on_recovery_request`, `on_requested_config`, commands) and robot-side code
decides how to meet it.

**No transcoding in Seyd.** The agent is a pure relay of encoded bytes. It may
depacketize RTP and split NAL units into chunks; it never decodes, re-encodes
or inspects media payloads beyond that.

**Whole frame or nothing.** A frame is admitted before its first chunk leaves
and then sent in full; a torn frame costs a GOP, a skipped frame costs a frame.
Keyframes are never dropped.

## Technology choices (current)

| Component | Stack | Rationale |
|---|---|---|
| Core (`packages/seyd-*`) | Rust, quinn + h3/h3-webtransport, rustls, rcgen | ADR 0002: only Rust stack with WebTransport; pure Rust → trivial ARM cross-builds and self-contained wheels |
| Agent form factors | C ABI via cbindgen; C++/Python(cffi)/ROS 2 wrappers; `seydd` daemon | ADR 0004 |
| Web pilot SDK | TypeScript, WebTransport, WebCodecs `VideoDecoder`, Web Worker + OffscreenCanvas | Presentation paced on the source's capture clock, bounded by a latency budget (ADR 0005); off-main-thread so host apps cannot jank video |
| Loss resilience | Reed-Solomon GF(256), Cauchy, per FEC block; NACK-driven LTR/intra-refresh/IDR recovery | FEC pays bandwidth, not round trips; recovery in one RTT for what FEC misses |
| Cloud | TypeScript (Fastify + ws), Postgres, Redis, OIDC (provider TBD), Docker | Portable by construction; `docker compose` runs the whole cloud |

## Hosting (current)

`cloud/api` runs on Google Cloud Run in `europe-west1`, project `seydio`:
`https://seyd-signal-flj7s44j4a-ew.a.run.app` (deploy with
`GCLOUD_PROJECT=seydio bash cloud/api/deploy.sh`; dev-mode auth). It is a plain
container on Postgres/Redis-shaped seams with the GCP dependency isolated to
the deploy script; `cloud/docker-compose.yml` is the portability proof.

## Running things — the scripts

| Script | What it does |
|---|---|
| `./demo-seyd.sh` | The camera demo robot: preflights the Hikvision over ISAPI, starts `seydd` + `examples/demo-robot/bridge.py` against the deployed cloud. Overrides: `CAMERA_IP` (CLI beats `.env.local`), `SIGNAL_URL`, `DARC_QOS_PROFILE`, `ROBOT_ID`. Needs `.env.local` (`CAMERA_USER`/`CAMERA_PASSWORD`). |
| `./sim-robot.sh` | Webcam robot (no camera needed): FFmpeg webcam + counter sensor + `seydd` as robot `seyd-sim` on the deployed cloud. `VIDEO_DEVICE=lavfi` for a synthetic source; same overrides as above. |
| `tools/seyd-smoke.py` | End-to-end assertion in headless Chrome (venv: `tools/.venv`, created by `tools/setup-machine.sh`). `--robot`, `--page`, `--signal`, `--no-sensor`, `--camera-ip <ip>` (verifies PTZ moved the real camera), `--query loss=0.05`, `--record N` (per-second stats to jsonl for field runs). |
| `tools/setup-machine.sh` | Bootstrap a fresh Mac (brew, node, pnpm, rustup, tools/.venv, first build). |

Both robot scripts `pkill` any running `seydd` and rebuild `target/release/seydd`
from the working tree first. Pilot pages: deployed landing at `/`, pilot at
`/pilot/?robot=<id>`; press `S` for the HUD. Field procedures: docs/field-test.md.

## Deploying the backend

Two Cloud Run services, project `seydio`, region `europe-west1` (memory +
docs/eu-hosting.md carry the context):

- **seyd-signal** (signal server + static demo pages):
  `pnpm -r build && GCLOUD_PROJECT=seydio bash cloud/api/deploy.sh`.
  The script bundles `web/demo/dist` into the image (`.gcloudignore` keeps it in
  the upload) and re-applies the prober env from `.env.local` after deploy
  (plain `--set-env-vars` would wipe it). URL:
  `https://seyd-signal-flj7s44j4a-ew.a.run.app`. Dev-mode auth (TOFU enrolment,
  anonymous pilots) — no real accounts yet.
  Deploy blips: presence is in-memory, so robots show offline until their
  WebSocket reconnects off the draining revision (≤ ~30 s; restart the robot to
  force it).
- **seyd-prober** (reachability probe, called by seyd-signal on every announce):
  build with Cloud Build from the REPO ROOT context —
  `gcloud builds submit --project seydio --config deploy/cloudbuild-prober.yaml .`
  then the `gcloud run deploy` command in that file's header. The
  `SEYD_PROBER_TOKEN` must match on both services; it lives in `.env.local`
  (gitignored).
- Logs: `gcloud logging read 'resource.type="cloud_run_revision" AND
  resource.labels.service_name="seyd-signal"' --project seydio ...` (pino JSON
  in textPayload; session events have `msg` like "session offered/accepted").

## Verifying work

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings
python3 tools/fec-vectors.py | cargo run -p seyd-fec --example check   # Rust ↔ Python FEC interop
pnpm -r build && pnpm -r test                                           # @seyd/core, @seyd/web, web/demo
(cd cloud/api && npm test)

# End to end on this machine (sim source, no camera):
(cd cloud/api && PORT=8080 SEYD_DEV_OPEN_ENROLMENT=1 SEYD_DEV_ALLOW_ANONYMOUS=1 \
   SEYD_STATIC_DIR=$PWD/../../web/demo/dist node dist/index.js &)
VIDEO_DEVICE=lavfi DARC_QOS_PROFILE=latency ./sim/video-source.sh & python3 sim/sensor-source.py &
./target/debug/seydd --config <a seydd.toml with rtp://127.0.0.1:5000, udp://127.0.0.1:5002, ptz → udp://127.0.0.1:5004> &
tools/.venv/bin/python3 tools/seyd-smoke.py --robot <robot_id> [--query loss=0.05]   # venv: tools/setup-machine.sh
```
Then open `http://localhost:8080/?robot=<robot_id>&signal=ws://localhost:8080/ws`
in Chrome. The real camera: `./demo-seyd.sh` (DEMO.md); verify with
`tools/seyd-smoke.py --robot seyd-demo --no-sensor --camera-ip <ip>`.

Field testing off the LAN: `docs/field-test.md`.

## What we do not build

The robot or vehicle, the camera or sensors, the control algorithms or
autopilot, a managed human-operator network, a transcoding layer.
