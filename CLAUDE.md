# Seyd — Repository Guide for Claude

## What this repo is

This is the public repository of **Seyd** (formerly DARC), the connectivity
layer for remote operation of autonomous vehicles and robots: hyper-low-latency,
point-to-point video, sensor and command streaming between robot systems and
human operators over the public internet, delivered as SDKs plus a signaling
cloud. Domains: `seyd.io`, `seydio.com`. Licensed under Apache-2.0
(`LICENSE`, `NOTICE`); contributions under the DCO (`CONTRIBUTING.md`).

The product is a Rust core with a C ABI, a TypeScript web pilot SDK, and a
portable cloud. This repository holds the core, the SDKs, the web pilot, the
developer docs, the examples and the harnesses. The hosted cloud (signal
server, relay, console, prober, deploy tooling) is the private repository
`seyd-io/seyd-cloud`, which pins this one as a submodule; its operations
guide is that repository's `CLAUDE.md`. Business planning lives in the
private `seyd-io/seyd-business`. One question routes a document: who needs
to read it (`docs/open-source.md`). The Python/JS proof of concept that
preceded the new stack has been deleted (git history before 2026-08-28 has
its agent and pilot; its signal server is in `seyd-cloud`'s history;
PROTOTYPE.md describes it); its measurements remain valid. `PLAN.md` is
the plan; follow it.

## Key documents — read these first

- **PLAN.md** — the approved product plan: fixed decisions, the technology
  upgrades that define "world-class", the target architecture, ordered work,
  and verification. Start here.
- **docs/adr/** — architecture decision records. ADR 0001 (wire protocol v2),
  0002 (quinn), 0003 (MoQ), 0004 (C ABI), 0005 (presentation pacing),
  0006 (loss measured on one clock), 0007 (identity: pluggable authn, our
  authz), 0008 (simulcast: adapt by selecting a stream, not reconfiguring
  one), 0009 (keyframes on demand: intra refresh or a long GOP, never a
  one-second IDR cadence), 0010 (cloud relay as the last resort: taken only
  after the direct race fails, always shown as relayed), 0011 (a publisher
  states its own bitrate ceiling: `max_bitrate_kbps` per video channel lowers
  the rate controller's range; never raises it). Add one for every
  decision of that weight; never change a wire format or public API without one.
- **SPEC.md** — product specification: vision, use cases, architecture,
  the no-transcoding principle, latency reality, technology choices. Written
  under the DARC name; the product decisions section at the top records what
  changed on 2026-08-28. Customers and competitors are in `seyd-business`.
- **PROTOTYPE.md** — the proof-of-concept build, frozen as history. Its
  measurements (FEC, NAT reachability table, decoder gotchas) remain valid
  inputs; its component descriptions describe the legacy Python/JS code only.
- **DEMO.md** — the always-on Hikvision PTZ demo. The first customer program
  the new stack must run.
- **docs/latency-roadmap.md** — ordered work on end-to-end latency, with the
  measured bar from the competitor survey. Read before touching the recovery,
  pacing or FEC paths.
- **docs/latency-sources.md** — every stage where a byte waits between sensor
  and screen, with what is measured and what is not. Read before claiming a
  latency number; the HUD's "g2g" is not glass-to-glass.
- **docs/encoder-setup.md** — how a publisher (x264, GStreamer, NVENC, Jetson,
  Hikvision, Axis, ONVIF) must be configured for Seyd, and how to verify it.
- **docs/design.md** — the design system: tokens, type, the meaning of each
  colour, light and dark mode, the shared components. `web/theme`
  (`@seyd/theme`) is the same system as code; every web surface imports it,
  and presentations copy its palettes. Read before touching anything a person
  sees.
- **web/docs** — the developer documentation site (PLAN.md §2.8): Starlight,
  served at `/docs/`. The API references are *generated from the code* by
  `tools/docs/` and starlight-typedoc; the guides import real example files.
  See "Developer documentation" below before changing any public surface.
- **DEMO-ROVER.md** — the planned second demo: a remotely driven rover in a
  booked, attended setting; v2 adds a second camera and a two-pilot
  driver/spotter model. Hardware not yet ordered.
- **docs/open-source.md** — the split between this public repository
  (Apache-2.0, headers, DCO, filtered history), the private `seyd-io/seyd-cloud`
  that pins it as a submodule, and the documents-only `seyd-io/seyd-business`.
  Read before adding a file that could belong to another of the three:
  anything about running the hosted cloud is `seyd-cloud`; anything about
  money, customers or competitors is `seyd-business`.
- **DEMO-TELLO.md** — the Tello drone demo: the drone's binary protocol as
  used, the bridge's safety rules (stick hold, orphan landing, altitude
  limit), the pilot's flight control scheme, and the bench checklist for the
  flight logs. Flown in the room and off the LAN; at the drone's range limit
  its own radio is the bottleneck (keyframes stop surviving it).

## Before starting a bigger task — check git first

**Run `git status` before beginning any substantial piece of work. If the tree
has uncommitted changes, stop and ask the owner what to do with them — commit,
branch, or something else — before writing a line.** Do not start work on top of
them and sort it out afterwards.

Two things go wrong otherwise, both observed while building simulcast
(ADR 0008) on top of an already-dirty tree:

- **The finished work cannot be committed cleanly.** The new feature touched
  `engine.rs`, `config.rs` and `seydd/src/main.rs`, which already held unrelated
  uncommitted work, and `main.rs` had come to depend on an untracked
  `enrol.rs`. The only remaining choices were a commit mixing several features
  or one that did not build.
- **Untracked files get destroyed silently.** `docs/adr/0008-simulcast.md` and
  `packages/seyd-qos/src/simulcast.rs` existed as untracked drafts and were
  overwritten; git had no copy, so they were gone. Read before overwriting, and
  remember that for untracked files there is no recovery at all.

## Fixed product decisions (2026-08-28) — do not re-open without the owner

- **Direct first; the cloud relay only as the last resort** (amended
  2026-09-09, ADR 0010). The candidate race always runs first. Only when it
  fails, and the robot allows it, does the pilot attach to the WebSocket relay
  on the signal server — and then the HUD, the status line and the guidance
  box all say the session is relayed and why the direct path failed. The
  relay is metered and separately priced; never make it the first choice, and
  never hide it.
- **Rust core + C ABI.** All protocol logic in Rust crates; every other agent
  form factor (C++, Python, ROS 2, daemon) is a thin wrapper with no protocol
  logic. See ADR 0004.
- **Web pilot SDK first.** Mobile SDKs when a customer needs them.
- **Portable cloud, EU residency likely, no Google lock-in.** Cloud Run in
  `europe-west1` today; plain containers on Postgres + Redis; no GCP-only SDKs
  or services in application code; auth behind an OIDC abstraction, provider
  not yet chosen.
- **No timeline/headcount planning.** Plans are ordered work.

## Commits are signed off

`main` requires the DCO check: every commit carries
`Signed-off-by: Name <email>`. Always commit with `git commit -s` (there is
no git config that adds it to ordinary commits; `format.signOff` only
affects `format-patch`). CONTRIBUTING.md explains it to outside
contributors; it applies to the owner's commits too, because the check does
not distinguish, and an admin's direct push bypasses the rule silently.

## Keeping documentation current — mandatory

When you make a decision that isn't reflected in PLAN.md, the ADRs or SPEC.md,
update the relevant document before moving on: technology choices, interface
changes, scope changes, discoveries, architectural pivots. The rule: **if a
future developer reading only PLAN.md, the ADRs and SPEC.md would be surprised
by the code, the docs are out of date.** Every measured number quoted in a doc
must be re-measured when the code it describes changes.

## Developer documentation — keep it generated, keep it current

`web/docs` is the public developer documentation (PLAN.md §2.8). Its
references are produced from the code on every build and its guides render
real example files, so the rule is mechanical:

- **A public surface is not changed until the docs build passes and the guide
  says the new thing.** The surfaces: `sdks/c/include/seyd.h` (regenerated from
  `seyd-ffi`'s `///` comments, which *are* the C reference), the `seyd` Python
  package's docstrings, the exports of `@seyd/core` and `@seyd/web` (TSDoc →
  TypeDoc), `packages/seydd/src/config.rs` (the `seydd.toml` reference), the
  QoS constants in `seyd-qos`, the `FailureClass` union in
  `<seyd-connect-error>` (one page per class under
  `web/docs/src/content/docs/networking/classes/`, enforced by
  `tools/docs/check-networking.py`), and **the integration skill
  `skills/seyd/`** (below).
- **The integration skill changes with every public surface.** `skills/seyd/`
  (`SKILL.md` + `references/`) is the document a third-party developer hands
  their coding agent to plan and build an integration: it interviews for what
  the plan needs, picks the form factor, and holds the rules of every part.
  It is served verbatim at `/docs/skill/` and explained on
  `web/docs/src/content/docs/start/agent-skill.mdx`. When you add or change a
  C function or callback, a Python `Agent` method or handler, a `<seyd-video>`
  attribute, a `SeydSession` event or failure reason, an `AgentEvent`
  variant, a publisher-control message, a failure class, a `seydd.toml` key,
  a QoS number, an example file or a docs page, say in the skill what an
  integrator does with it, in the same commit. `tools/docs/gen-skill.py`
  regenerates its two generated references (`seydd-config.md`,
  `qos-profiles.md`); `--check` runs in the docs build and fails on an
  unmentioned surface, a dead path or route, or a stale generated file. A
  change in *behaviour* (a rule, a default, a measured number) is not caught
  by the check: re-read the reference that states it.
- **Doc comments are the documentation.** A new field, function or event
  without a `///`, a docstring or a TSDoc comment renders as a blank row on
  the site; write the comment where the code is.
- **Examples are files, never code blocks in a page.** Put them under an SDK's
  `examples/` (C examples are built by `make -C sdks/c`; the TypeScript ones
  are type-checked by the docs build; `seyd-core` examples by `cargo build
  --examples`) and import them into MDX with `@repo/…?raw`.
- **A guide that explains a changed surface changes with it**, in the same
  commit. The guides live under `web/docs/src/content/docs/`.
- `pnpm --filter docs build` is part of verification (`pnpm -r build` runs
  it). `SEYD_DOCS_SKIP_RUSTDOC=1` skips the slow `cargo doc` step locally; the
  deploy must not skip it.

## Monorepo structure

```
seyd/
├── CLAUDE.md  PLAN.md  SPEC.md  PROTOTYPE.md  DEMO.md  README.md  LICENSE  NOTICE  CONTRIBUTING.md  SECURITY.md
├── Cargo.toml                 # Rust workspace
├── packages/                  # Seyd core — Rust, production
│   ├── seyd-wire/             # wire protocol v2 headers + legacy v1 decode (ADR 0001)
│   ├── seyd-fec/              # Reed-Solomon GF(256), Cauchy; must pass tools/fec-vectors.py
│   ├── seyd-qos/              # QoS profiles + closed-loop ABR controller (pure)
│   ├── seyd-nat/              # STUN, NAT classification, PCP/NAT-PMP/UPnP, candidates, NatReport
│   ├── seyd-transport/        # quinn: WebTransport (h3) + native QUIC (seyd/2); block sender
│   ├── seyd-signal-client/    # WS client to the cloud, signal v2, Ed25519 robot auth
│   ├── seyd-core/             # Agent (lifecycle) + engine: channels, sessions, events
│   ├── seyd-ffi/              # C ABI (cdylib/staticlib) → sdks/c/include/seyd.h
│   └── seydd/                 # daemon: TOML config, RTP/RTSP/UDP inputs
├── sdks/                      # thin wrappers — NO protocol logic here, ever
│   ├── c/       # generated seyd.h, Makefile, abi-smoke + sensor-robot examples
│   ├── python/  # cffi ABI mode over libseyd; cpp/ and ros2/ not built yet
│   └── js/core  js/web                # @seyd/core (session API), @seyd/web (<seyd-video>, <seyd-hud>, <seyd-connect-error>); examples/ in each
├── skills/seyd/               # the integration skill for third-party developers' coding agents (SKILL.md + references/); served at /docs/skill/; checked by tools/docs/gen-skill.py
├── web/theme      # @seyd/theme: design tokens, base styles, self-hosted fonts, theme switch (docs/design.md)
├── web/demo       # the landing page (/) and the pilot page (/pilot/)
├── web/docs       # developer docs (/docs/): Starlight; references generated from the code (PLAN.md §2.8)
├── docs/                      # ADRs (docs/adr/), protocol contracts (docs/protocol/), the hand-written docs the site imports
├── .github/                   # CI (the verification set below), Dependabot, issue templates
├── examples/demo-robot/       # the Hikvision PTZ demo as a customer program
├── examples/tello-robot/      # the Tello drone demo: tello.py (protocol), h264rtp.py (Annex B → RTP), bridge.py, fake_tello.py; host/ = the same robot as a native Rust host of seyd-core (workspace member)
├── sim/                       # robot simulation — NOT part of Seyd
│   ├── video-source.sh        # FFmpeg webcam → RTP/H.264 UDP :5000 (owns encoder settings; intra refresh by default)
│   └── sensor-source.py       # counter → UDP :5002 at 10 Hz
├── demo-start.sh              # find the camera, then start the camera demo robot and wait for it online
├── demo-seyd.sh               # start the camera demo robot
├── demo-tello.sh              # start the Tello drone demo robot (--fake: simulated drone on localhost)
└── tools/                     # harnesses: seyd-smoke.py, cdp.py, latency-ab.py + link-shaper.py + keyframe-probe.py, fec-vectors.py + fec-reference/, find-camera.py, setup-machine.sh
    └── docs/                  # the docs generators: gen-c-reference.py, gen-python-reference.py, gen-seydd-config.py, gen-qos-profiles.py, check-networking.py, gen-skill.py
```

Not here, by design: `cloud/` (signal server, relay, console API, Logto,
compose), `cloud/prober`, `web/console` and `deploy/` are in
`seyd-io/seyd-cloud`. The protocol those speak is documented here
(`docs/protocol/`, the *Enrolment and access* docs page) because the SDKs
speak it.

**Import direction:** `tools/` may reach into `packages/`. `packages/` and
`sdks/` must never reach into `tools/` or `sim/`. `sdks/` may only call the
C ABI or `@seyd/core`; if a wrapper grows a parser or a heuristic, move it into
a crate.

## Where QoS settings live

Encoder settings (resolution, preset, VBV, GOP) belong to the robot's video
publisher — `sim/video-source.sh` in the simulation, the camera in the demo.
Seyd states only transport-observable *targets* (bitrate ceiling, latency
budget, max GOP, intra-refresh preference) via `on_requested_config`, plus its
own transport and pilot policy (FEC rate, drop threshold, close-out deadlines,
presentation delay). Keyframes are on demand (ADR 0009): `maxGopMs` is a long
safety net, and the publisher must answer `recovery-request`. If
you find yourself putting a resolution in `seyd-qos`, or an FEC percentage in
`sim/`, the boundary has leaked.

## Component philosophy

**Loose coupling.** Components communicate only through defined interfaces:
the wire protocol, the signal protocol, the C ABI, UDP sockets.

**One agent lifecycle, many hosts.** `seyd_core::Agent` owns everything between
a configuration and a running robot: sockets, discovery, certificate, endpoint,
engine, signaling, lease renewal, cert rotation, network-change re-gather. A
*host* supplies media and consumes `AgentEvent`, and owns only what differs by
form factor — `seydd` owns TOML, RTP/RTSP inputs and UDP sinks; `seyd-ffi` owns
C marshalling. If you find yourself adding lifecycle code to a host, it belongs
in `Agent`, or `seydd` and the SDKs will drift.

**High cohesion.** Each crate does one thing. `seyd-wire` is layout only;
`seyd-fec` never parses a header; `seyd-qos` does no I/O.

**Production-boundary awareness.** `sim/` keeps fake robot code out of Seyd.
Vendor drivers (the Hikvision ISAPI code in `examples/demo-robot/hikvision.py`)
belong in `examples/`, not in a Seyd component — the agent states intent
(`on_recovery_request`, `on_requested_config`, commands) and robot-side code
decides how to meet it.

**Authentication is pluggable; authorization is the product.** Seyd has three
identity planes and only one touches an identity provider. Robot identity
(Ed25519, enrolment tokens) and pilot session tokens (ES256, our key) are ours
and must stay that way — a fleet keeps working while the IdP is down. Human
identity comes from OIDC, behind one seam in the cloud (`authn/` in
`seyd-cloud`), which is the only place that knows how a person proves who
they are. Everything downstream sees a `Principal`. Orgs, roles, robot grants
and the audit log live in the cloud's own `accounts/` and Postgres, because
no IdP can express "may drive robot 42 but only observe robot 7". If you
find yourself reaching for a provider-specific SDK, or storing an IdP
concept where authorization lives, the boundary has leaked. See ADR 0007
and `docs/self-hosting-auth.md`.

**No transcoding in Seyd.** The agent is a pure relay of encoded bytes. It may
depacketize RTP and split NAL units into chunks; it never decodes, re-encodes
or inspects media payloads beyond that. Adaptation obeys the same rule: where a
publisher offers several encodings of one picture, the agent *chooses which
already-encoded stream to forward* (simulcast, ADR 0008) rather than changing
the video. Degrade resolution first and frame rate last — for a remote pilot,
frame interval is latency, not quality.

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
| Cloud | TypeScript (Fastify + ws), Postgres, Redis (not yet), OIDC via self-hosted Logto, Docker | Portable by construction; `docker compose` runs the whole cloud (ADR 0007) |

## Running things — the scripts

| Script | What it does |
|---|---|
| `./demo-start.sh` | "Find a camera and start the demo" in one go: probes `CAMERA_IP`, falls back to `tools/find-camera.py` discovery, starts `./demo-seyd.sh` with the address that answered, waits until the cloud lists the robot online and prints the pilot URL. `--detach` leaves it running; otherwise Ctrl-C stops it. Log in `$DEMO_LOG` (default `$TMPDIR/seyd-demo.log`). |
| `./demo-seyd.sh` | The camera demo robot: preflights the Hikvision over ISAPI, starts `seydd` + `examples/demo-robot/bridge.py` against the deployed cloud. Overrides: `CAMERA_IP` (CLI beats `.env.local`), `SIGNAL_URL`, `DARC_QOS_PROFILE`, `ROBOT_ID`. Needs `.env.local` (`CAMERA_USER`/`CAMERA_PASSWORD`). |
| `./demo-tello.sh` | The Tello drone robot (DEMO-TELLO.md): checks the laptop is on the drone's Wi-Fi with the default route elsewhere, then `seydd` + `examples/tello-robot/bridge.py`. `--fake` runs `fake_tello.py` instead of a drone; `--rust` runs the native host (`examples/tello-robot/host`) instead of `seydd` + the bridge. `BRIDGE_ARGS=--no-takeoff` for bench work. |
| `./sim-robot.sh` | Webcam robot (no camera needed): FFmpeg webcam + counter sensor + `seydd` as robot `seyd-sim` on the deployed cloud. `VIDEO_DEVICE=lavfi` for a synthetic source; same overrides as above. |
| `tools/seyd-smoke.py` | End-to-end assertion in headless Chrome (venv: `tools/.venv`, created by `tools/setup-machine.sh`). `--robot`, `--page`, `--signal`, `--no-sensor`, `--camera-ip <ip>` (verifies PTZ moved the real camera), `--query loss=0.05`, `--record N` (per-second stats to jsonl for field runs), `--command flight` for a robot with a `flight` channel instead of `ptz`. |
| `tools/setup-machine.sh` | Bootstrap a fresh Mac (brew, node, pnpm, rustup, FFmpeg, tools/.venv, first build). |

Both robot scripts `pkill` any running `seydd` and rebuild `target/release/seydd`
from the working tree first. They talk to the hosted cloud
(`wss://seyd-signal-flj7s44j4a-ew.a.run.app/ws`), where a robot needs an
enrolment token (`ENROL_TOKEN=seyd_enr_… ./sim-robot.sh` redeems one; the
robot's key file is what stays enrolled) and a pilot without an account
reaches only robots with a public grant. Tokens and grants come from the
console or from the operator of the cloud. Pilot pages: deployed landing at
`/`, pilot at `/pilot/?robot=<id>`; press `S` for the HUD. `?paths=none`
forces the direct race to fail so the relay path can be exercised; `?relay=0`
refuses the relay. Field procedures: docs/field-test.md. Running the cloud
itself, locally or deployed, is `seyd-cloud`.

## Verifying work

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings
python3 tools/fec-vectors.py | cargo run -p seyd-fec --example check   # Rust ↔ Python FEC interop
make -C sdks/c check                                                    # C ABI conformance (abi-smoke)
tools/.venv/bin/python3 -m pytest sdks/python/tests                     # Python SDK over libseyd
pnpm -r build && pnpm -r test                                           # @seyd/core, @seyd/web, web/demo, web/docs (generates the references and checks the skill; SEYD_DOCS_SKIP_RUSTDOC=1 to skip cargo doc locally)
python3 tools/check-headers.py                                          # every source file carries the license header
cargo deny check licenses                                               # dependency licenses stay within deny.toml

# End to end on this machine (sim source, no camera), against the hosted cloud:
VIDEO_DEVICE=lavfi ./sim-robot.sh                                       # robot seyd-sim; first run needs ENROL_TOKEN=…
tools/.venv/bin/python3 tools/seyd-smoke.py --robot seyd-sim [--query loss=0.05]   # venv: tools/setup-machine.sh
```
`.github/workflows/ci.yml` runs the same set on every pull request. The real
camera: `./demo-seyd.sh` (DEMO.md); verify with
`tools/seyd-smoke.py --robot seyd-demo --no-sensor --camera-ip <ip>`. A
local cloud (no accounts, no tokens) is `seyd-cloud`'s loop.

Field testing off the LAN: `docs/field-test.md`.

## What we do not build

The robot or vehicle, the camera or sensors, the control algorithms or
autopilot, a managed human-operator network, a transcoding layer.
