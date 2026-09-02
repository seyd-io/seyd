# Seyd — Product Specification

Seyd (formerly DARC; renamed 2026-08-28) is the product this repository builds.
This document is the product specification: who it is for, what it is and is
not, and the technology positions with the measurements behind them. The
ordered work lives in PLAN.md; individual decisions of architectural weight
live in docs/adr/; wire-level contracts in docs/protocol/; field results in
docs/field-test.md. Where this document and the code disagree, one of them is
wrong and must be fixed — see CLAUDE.md's documentation rule.

## Product Vision

Seyd is a software-as-a-service platform that enables low-latency, secure,
peer-to-peer remote operation of autonomous vehicles and robots over the
internet. Seyd is not the robot, the pilot system, or the control UI — it is
the connectivity layer: a set of drop-in software components that any robot or
vehicle system can integrate to gain remote-operation capability.

Fixed product positions (owner decisions, 2026-08-28):

- **Point-to-point only.** Media never touches Seyd's servers. When a direct
  connection cannot be made, the pilot explains *why* (NAT type, port-mapping
  result, IPv6 presence, reachability probe) and how to fix it. Relays are a
  future, separately priced tier — designed for, not built.
- **One Rust core with a C ABI** (ADR 0004) on the quinn QUIC stack (ADR
  0002). Every other agent form factor — daemon, C++, Python, ROS 2 — is a
  thin wrapper containing no protocol logic.
- **Web pilot SDK first.** iOS/Android/Flutter are built when a customer needs
  them.
- **Portable cloud, EU residency likely, no Google lock-in.** Plain containers
  on Postgres + Redis shapes; auth behind an OIDC abstraction with the
  provider deliberately not yet chosen; hosted on Cloud Run in `europe-west1`
  today with the GCP dependency isolated to the deploy script.
- **Plans are ordered work,** not schedules.

---

## Problem Statement

Operating autonomous vehicles and robots remotely across large distances requires:

- **Very low-latency video** from the vehicle to the operator (ideally <100ms glass-to-glass)
- **Reliable command delivery** from the operator to the vehicle
- **Sensor data streaming** from the vehicle (LiDAR, IMU, GPS, ultrasonic, etc.)
- **Secure, authenticated connections** — a rogue operator must never be able to take over a vehicle
- **Discoverability** — operators need to find and connect to specific vehicles in a fleet
- **Scalability** — a single operator may manage a fleet; a fleet may have thousands of vehicles

General-purpose video conferencing tools are not designed for this: they optimize for human perception, not minimal latency and deterministic delivery. Industrial SCADA systems are closed, expensive, and not cloud-native.

**The specific gap in the market:** Every existing solution is either a full-stack product (they own the operator UI and sometimes the operator network too), a general-purpose communications platform adapted for robotics, or purpose-built technology that has been acquired and internalized. There is no pure connectivity-layer SaaS that robot and vehicle makers can drop into their existing system and own the integration end-to-end.

---

## Customer Archetypes

Understanding who integrates Seyd shapes every product decision.

### Archetype A — "I have a robot, I need a pilot app"
Building from scratch or operating a simple system. Wants Seyd to handle as much as possible: video delivery to a browser, embeddable UI components, sensor widgets. Will struggle with codecs, transport negotiation, and mobile integration if left to figure it out alone. **These customers want Seyd to be opinionated.** Served today by `<seyd-video>` and the `SeydSession` API.

### Archetype B — "I have a pilot system, I need it to work over the internet"
Already has a desktop pilot application. It reads UDP from a LAN port and renders video natively. Their robot already publishes RTP or a proprietary stream. They don't want Seyd to touch their pipeline — they want Seyd to make `robot-lan:5000` appear as `localhost:5000` on the pilot machine, securely, over the internet, with low latency. **These customers want Seyd to be invisible.**

The mental model for Archetype B: **Seyd is a VPN for robot LAN data.** The pilot application never knows it isn't on the same LAN as the robot. On the robot, `seydd`'s `bytes-udp` channels carry raw datagrams; on the pilot machine, the planned headless pilot agent re-emits them on `localhost` (video as RTP/H.264, so an existing GStreamer/FFmpeg pipeline reads exactly what it read on the LAN).

Both archetypes ride the same Seyd transport layer. The difference is how much of the stack above the transport Seyd owns.

### Where these archetypes live (target verticals)

The highest-pain version of this problem is **occasional remote operation of a
vehicle that is normally autonomous or locally operated**, on networks nobody
controls end to end:

- **AV prototypes and test rigs** — early-stage autonomous vehicles where a
  human must be able to take over convincingly, and proof-of-concept programs
  for driving vehicles and drones at a distance (including military test
  ranges). The takeover path is bought before the autonomy is trusted.
- **ROVs and remotely operated machinery** in oil & gas, subsea and defense.
  Note the topology: a subsea ROV is tethered to a surface vessel, so Seyd's
  "robot side" is the vessel — and the vessel's uplink is typically satellite
  (see docs/starlink.md, which is directly about this link).
- **Delivery/logistics robot fleets** needing rescue-and-supervise (the
  classic Archetype A).

These customers are predominantly Archetype B: they already own a command &
control (C2) stack and want the internet leg solved. Defense and industrial
buyers also make the portable-cloud decision load-bearing — the whole cloud
runs from `docker compose` on their infrastructure, with no hyperscaler
dependency in application code.

### C2 systems, pub/sub middleware and DDS

Archetype-B C2 stacks are typically built on LAN pub/sub middleware — most
often **DDS** (which is also what ROS 2 speaks underneath). Two facts shape
Seyd's role there:

1. **DDS does not cross the public internet by itself** — discovery is
   multicast, transports assume a LAN, and NAT traversal is somebody else's
   problem. Existing WAN bridges (routing services, DDS routers) forward
   topics over generic TCP/UDP with no media awareness and a weak
   reachability story.
2. **Video must not be treated as just another topic.** A reliable-QoS video
   topic over a lossy link stalls like TCP; a best-effort one smears for
   seconds on every loss. Neither repairs without a round trip, bounds
   latency, or asks the encoder to adapt.

Seyd's position: **act as the domain relay for the real-time plane.** Selected
command and sensor topics map onto Seyd's unreliable channels (sequence
numbers, age-out, FEC where wanted) via a thin DDS adapter — an SDK wrapper
over the C ABI or a `seydd` channel, per the no-protocol-logic-in-wrappers
rule, exactly like the ROS 2 node. Video bypasses the middleware entirely and
rides Seyd's media path from the camera stream, because that path exists
precisely to do what pub/sub cannot: minimum-latency video over a lossy
internet, always. Bulk topics (mission plans, map updates, file transfer) are
explicitly out of scope — they belong on a boring reliable channel (HTTPS,
rsync, the C2's own tools), not on a real-time plane.

---

## What Seyd Is (and Is Not)

| Seyd provides | Seyd does not provide |
|---|---|
| Peer-to-peer video tunnel | The camera or video encoder |
| Sensor data channels | The sensors or sensor fusion |
| Command channels with driver/observer roles | The control algorithms or autopilot |
| Vehicle discovery & fleet presence | The vehicle hardware |
| Session authentication & authorization | The operator UI (except optional SDK components) |
| Connection-quality hooks and closed-loop rate control | The fleet management business logic |
| Reachability diagnosis when P2P is impossible | Managed human operators |
| Relay fallback (future paid tier — not in v1) | Transcoding or format conversion |
| The real-time plane: video, commands, sensor streams | Bulk transfer: mission plans, logs, software updates |

Seyd exposes SDKs and APIs. Integrators build the robot agent and the operator application on top.

---

## Core Use Cases

### 1. Teleoperation (Direct Control)
An operator takes full manual control of a vehicle in real time. Latency is critical — commands and video must be synchronized to avoid disorientation and accidents.

- **Example:** A delivery robot is stuck; a remote operator steers it around the obstacle. An AV prototype leaves its envelope on a test range; the safety operator takes over from the control room.
- **Latency target:** <100ms end-to-end (achievable on continental connections; ~150–200ms intercontinental is realistic physics-limited floor). Measured in the field: 17–31 ms median glass-to-glass over one and two cellular hops (see Latency Reality).

### 2. Supervisory Control
An autonomous vehicle operates on its own but streams video and sensor data to an operator dashboard. The operator monitors and can intervene at any time.

- **Example:** An autonomous truck drives a highway route; a dispatcher watches 10 vehicles simultaneously.
- **Latency target:** <200ms acceptable; operator is not actively steering
- Served by driver/observer roles: many observers, one driver, enforced at the robot.

### 3. Assisted Takeover (Vehicle-Initiated)
The vehicle's onboard system detects a situation it cannot handle and signals the operations center. An available operator is dispatched to the session.

- **Example:** A robot flags "low confidence" in its perception stack; the platform alerts an operator.
- **Key requirement:** Vehicle-side "help request" API; operator assignment/queue system

### 4. Fleet Monitoring
An operator views a dashboard of all active vehicles — location, health, stream thumbnails — without necessarily being connected to any single vehicle in detail.

- **Key requirement:** Lightweight presence/telemetry channel (not full video for all vehicles simultaneously). The deployed landing page's live robot list is the first slice of this, fed by the same presence stream.

### 5. Multi-Vehicle Takeover
A single operator is connected to more than one vehicle at once, switching focus between them.

- **Key requirement:** Session multiplexing; operator can have N sessions open simultaneously

---

## Architecture Overview

```
┌─────────────────────────────────────────────┐
│               Seyd cloud (EU)               │
│  signaling (WS, /ws) · robot identity ·     │
│  presence · landing page + pilot at /pilot/ │
│  — never carries media —                    │
└─────────────────────────────────────────────┘
         │  signaling only            │
         │  (WSS)                     │
    ┌────▼──────┐              ┌──────▼───────┐
    │ seydd /   │◄────────────►│  @seyd/core  │
    │ libseyd   │  QUIC        │  <seyd-video>│
    │ (Rust, on │  datagrams,  │  (browser,   │
    │ robot or  │  direct,     │  WebTransport│
    │ LAN gw)   │  TLS 1.3     │  + WebCodecs)│
    └───────────┘              └──────────────┘
         │                            │
    camera · encoder ·          canvas · HUD ·
    sensors · actuators         controls
```

### Components

#### Robot side — `seydd` daemon and `libseyd`
- Runs on the robot, vehicle edge computer, or a LAN gateway co-located with the vehicle.
- Forwards encoded video byte-for-byte (RTSP and RTP/UDP inputs built in; the library takes access units or single NAL units directly), relays sensor datagrams, delivers driver commands to generic UDP outputs or callbacks.
- Owns discovery and reachability: STUN + NAT classification, PCP/NAT-PMP/UPnP port mapping, IPv6 candidates, hole-punch probes, candidate advertisement, `NatReport`.
- Speaks *intent* to robot-side code, never vendor protocol: `video-config` (bitrate ceiling, latency budget, max GOP, suggested fps, reason), `recovery-request` (keyframe needed), `session` (park your actuators). See docs/protocol/seydd.md. The Hikvision driver in the demo lives in `examples/demo-robot/`, outside Seyd, as the pattern intends.
- Form factors: `seydd` (TOML-configured daemon, shipped) → C ABI `seyd.h` via `seyd-ffi` → Python (cffi wheels) → ROS 2 (`seyd_ros` rclcpp component) — the ROS 2 answer is "both": the node is a thin wrapper over the same core the daemon uses (planned; PLAN.md milestone C).

#### Pilot side — `@seyd/core` and the web components
- `SeydSession`: signaling, candidate race, per-block reassembly and FEC recovery, WebCodecs decode, clock sync, stats — run in a Web Worker rendering to an OffscreenCanvas so the host application cannot jank video (inline fallback where unavailable).
- `<seyd-video>`, `<seyd-hud>`, `<seyd-connect-error>`: drop-in custom elements; React bindings over the same core.
- Events for the integrator: `frame`, `sensor`, `link` (quality + degraded-picture flag), `stats`, `p2p-failed` (with the robot's NatReport and per-class mitigation guidance).
- Planned: headless pilot agent (the Archetype B `localhost` tunnel) on a native QUIC client, which also enables **direction-agnostic QUIC** — when the robot is unreachable but the pilot side is reachable, the roles of QUIC client and server swap while the application roles stay put. A browser can never do this; a native pilot can.

#### Seyd cloud (signaling & registry)
- **Signaling server** (`cloud/api`, signal protocol v2 — docs/protocol/signal-v2.md): brokers connection setup without touching media; assigns driver/observer roles; forwards NAT-punch requests; ends sessions whose P2P attempt failed so slots free immediately.
- **Robot identity:** Ed25519 key pair generated on the robot at first run; challenge-response authentication on every connection. Dev mode enrols unknown robots on first sight (TOFU); production enrolment tokens are planned.
- **Presence:** robots heartbeat every 5 s; the landing page shows the live fleet.
- **Auth service** (planned): OIDC login, orgs/RBAC in Postgres, short-lived ES256 session tokens minted by Seyd and verified *by the robot* before any command channel opens.
- **Relay (future tier):** not part of v1. Sessions that cannot go direct fail with a diagnosis.

---

## Data Channels

Channels are declared by the robot (`seydd.toml` or the API) and announced to
the pilot in the session handshake. All ride one QUIC connection. Wire format:
docs/protocol/chunks.md (ADR 0001).

### Video channels
- Direction: vehicle → operator. Payload: H.264 Annex B access units, forwarded byte-for-byte.
- Carried as QUIC datagrams in FEC blocks (8 data chunks + Reed-Solomon parity per block), with per-chunk send timestamps and per-frame capture timestamps for real glass-to-glass measurement.
- Whole frame or nothing: a frame is admitted before its first chunk leaves and then sent in full; keyframes are never dropped; a bounded in-order queue with keyframe flush prevents latency accumulation.
- Multiple video channels (multi-camera) are representable in the wire format; fan-out to many sessions is implemented.

### Sensor channels
- Direction: vehicle → operator, continuous. One UDP datagram in = one message out, per-channel sequence numbers, unreliable (`json` codec today; binary codecs by declaration).

### Command channels
- Direction: operator → vehicle. Datagrams with per-channel sequence and sender timestamps so the robot can age out stale commands (`max_command_age_ms`).
- **Only the driver's commands are delivered** — observer sessions are read-only, enforced at the robot, and the pilot UI shows the role.
- Reliable stream-carried variants for discrete commands are specified, not yet built.

### Control stream
- One bidirectional QUIC stream per session (docs/protocol/control-stream.md): hello/welcome, session token, clock sync (ping/pong), loss reports, keyframe requests, QoS changes, 1 Hz stats in both directions.

### Presence/Signaling channel
- Vehicle ↔ cloud WebSocket: challenge/auth, candidate announcement with NatReport, heartbeats, session lifecycle. The cloud stores and forwards; it has no opinion about payloads.

---

## Security Model

Current state, with dev-mode gaps stated plainly:

- All P2P media and data ride QUIC under TLS 1.3. Certificates are self-signed ECDSA P-256, ≤14-day validity, with the fingerprint pinned through the signaling channel (`serverCertificateHashes`) — no CA, no per-robot DNS.
- The Seyd cloud never sees media — it brokers connection setup only.
- Robots hold an Ed25519 identity generated locally at first run; the cloud verifies a challenge signature on every connection and pins the key to the robot id. **Dev mode** currently enrols unknown ids on first sight and accepts token-less pilots as anonymous; the demo robot is deliberately public.
- Designed and specified, not yet enforced: OIDC operator login, org RBAC, short-lived ES256 session tokens carried in the P2P handshake and verified by the robot before any command channel opens, revocation over signaling. This is the gate before any non-demo robot is exposed.

---

## Latency Reality

Understanding the true latency budget is critical for product positioning.
The prototype-era analysis holds; it is now backed by field measurements of
the shipped stack (2026-08-30/31, Hikvision 720p25 demo, docs/field-test.md):

| Path | RTT | Glass-to-glass p50 / p95 | True loss |
|---|---|---|---|
| LAN (`host`) | ~1 ms | **<1 ms / 3 ms** | 0 |
| Pilot on 4G hotspot, robot on office LAN (`srflx`, hole-punched) | 21–44 ms | **17–25 ms / 50 ms** | 0.0–0.4 % |
| Pilot on 4G hotspot, robot behind 4G router (`portmap`, double cellular) | 51 ms | **31 ms / 59 ms** | 0.0 % |

Glass-to-glass here is measured, not estimated: capture and send timestamps
travel in the wire format and the pilot syncs clocks over the control stream.
The p95 tail is dominated by keyframe serialization on thin uplinks (a
30–60 KB IDR over ~2 Mbps is ~200 ms of wire time) — the motivation for the
planned LTR/intra-refresh recovery (PLAN.md §1.2).

**Realistic positioning targets:** network RTT/2 plus a few milliseconds of
software. Continental 70–120 ms glass-to-glass; intercontinental 150–250 ms,
physics-limited — no protocol choice changes that materially.

**Cellular networks:** loss is transient (handovers), not congestion, and
jitter is bursty. Backing off on loss is wrong, and treating jitter as loss is
wrong. Seyd runs BBR with pacing, adapts its close-out deadlines to measured
jitter, and separates "frame timed out but chunks arrived late" from genuine
loss — all field-tuned (see Loss Resilience).

---

## Technology Choices

### Video Format Principle: No Transcoding in Seyd

The agent is a pure relay — it forwards H.264 bytes without decoding,
re-encoding, or inspecting media payloads. It may depacketize RTP and split
NAL units into chunks; that is framing, not transcoding.

The responsibility for codec compatibility sits with the robot-side publisher. H.264 Baseline is universally supported in every browser, every OS, and every mobile platform. If the robot publishes H.264 Baseline, any pilot — browser, native desktop, mobile — works with zero transcoding. This is the recommended default.

For robots that output an incompatible format (H.265, raw YUV, proprietary codec): an optional **transcoding sidecar** — a standalone container the customer runs on their own infrastructure between the robot and the agent — keeps Seyd itself out of the encoding business and off the latency path.

### Why WebRTC Is Wrong for Teleoperation

WebRTC was designed for video calls, where smooth playback is more important than minimum latency. For teleoperation, this is exactly backward.

**The jitter buffer problem.** WebRTC's browser jitter buffer delays frame delivery to absorb network jitter. It is controlled by the browser, not the application — you can hint at a target but cannot zero it out. The effective floor is 50–200ms. For a teleoperation control loop, this is catastrophic: an operator sees the world as it was 50–200ms ago, then sends a command, then waits for the vehicle to respond.

**The double encode/decode problem.** A WebRTC robot agent must decode the incoming H.264 stream to attach it to a media track — which then re-encodes it for delivery. Two full encode/decode cycles before the browser's own decode; each adds ~10ms and degrades image quality.

**Latency comparison (LAN, measured on the prototype and confirmed on the Seyd stack):**

| Stage | WebRTC + re-encode | Seyd (QUIC + WebCodecs) |
|---|---|---|
| H.264 encode | ~10ms | ~10ms |
| Decode + re-encode for WebRTC | ~20ms | eliminated |
| Jitter buffer | 50–200ms | 0ms |
| H.264 decode | ~5ms | ~5ms |
| **Total (software)** | **~85–235ms** | **~15ms** |

WebCodecs replaces the jitter buffer with a call to `decoder.decode()` — frames are rendered as they arrive (`optimizeForLatency: true`, `hardwareAcceleration: 'no-preference'`). The single encode chain is: publisher H.264 encode → passthrough → WebCodecs decode. Nothing in between.

### Transport: Rust core on quinn, WebTransport to the browser

The transport is a Rust workspace (`seyd-wire`, `seyd-fec`, `seyd-qos`,
`seyd-nat`, `seyd-transport`, `seyd-signal-client`, `seyd-core`, `seydd`) on
**quinn** — the only Rust QUIC stack with a WebTransport server layer, pure
Rust throughout, so ARM cross-builds (Jetson, RPi 5, RK3588) and
self-contained Python wheels are mechanical (ADR 0002). BBR with pacing on the
media path; DPLPMTUD enabled (raising the 1000-byte chunk payload toward the
path MTU is planned work).

Two ALPNs share one socket and one wire format: `h3` for browser WebTransport
(CONNECT to `/seyd`, cert pinned via `serverCertificateHashes`) and the native
`seyd/2` for non-browser peers (planned: headless pilot agent, agent↔agent),
which skips HTTP/3 framing and enables direction-agnostic connection setup.

**Wire protocol v2** (ADR 0001, docs/protocol/chunks.md): a 20-byte chunk
header carrying channel id, per-channel frame id, FEC block geometry
(`n`/`k`/`block_idx`/`end_of_frame`), true last-chunk length, chunk size, and
a send timestamp; optional per-frame metadata (capture timestamp) in the first
chunk. Blocks leave the robot as soon as eight chunks exist, so a large
keyframe starts on the wire before the encoder has finished producing it. The
version nibble makes format changes a hard cutover: unknown versions are
counted and dropped, never guessed at.

The Python/JS proof of concept that validated this architecture end-to-end
(including the WebSocket-relay training wheels) is deleted; PROTOTYPE.md
preserves it and its measurements as history. Its FEC coders survive as the
byte-for-byte conformance oracle (`tools/fec-reference/`, 55 interop vectors
that the Rust and TypeScript implementations must pass).

**Browser support: Chromium only.** The web pilot SDK supports Chrome, Edge
and other Chromium browsers on desktop and Android; **Chrome on an Android
handset is verified working (2026-09-02)**, so a phone operator is a supported
case, not a theoretical one. **Safari and every browser on iOS are
unsupported**, and iPhones and iPads cannot run the web pilot at all — App
Store rules make every iOS browser WebKit, so Chrome for iOS fails identically
to Safari.

One caveat on the supported phone case: only pan and tilt are reachable by
touch. Zoom is bound to the wheel and `+`/`-`, recentre to `H`, the fine/fast
modifier to `Shift`, and the HUD to `S` — all keyboard or mouse. A touch
operator can steer but cannot zoom. Tracked as Milestone B item 16.

The cause is `serverCertificateHashes`, which is Chromium-only (shipped in
Chrome 100; Firefox in progress with no ETA, Safari TBD). The robot self-signs
a short-lived ECDSA P-256 certificate whose SANs are its candidate *IP
addresses* (`seyd-transport/src/cert.rs`), and the pilot pins it by SHA-256
fingerprint rather than by name — that is exactly what lets one certificate
serve every candidate URL, all of which are IP literals. A browser without
fingerprint pinning has no way to trust it. WebTransport itself reached
Baseline in March 2026 (Safari 26.4), so the page loads and the API exists;
only the pinning option is missing.

Verified on 2026-09-02 against the live demo robot: an iPhone on 5G could not
connect, while a laptop tethered to that same iPhone connected normally over
the identical carrier path. The robot logged no `session started` and no QUIC
connection from the handset, so the failure is at or before the WebTransport
handshake, not in the decoder.

The fallback design remains CA-signed per-robot certificates under a domain we
control (DNS-01), but it is **not being built** — it forces every candidate URL
to become a hostname, which likely loses the LAN `host` candidate to
DNS-rebinding protection, puts DNS caching in the path of network-change
re-gather, and (if read as a single shared `*` certificate) would put one
private key on every robot. **iOS and Android are served by the native SDKs in
Milestone C instead**; those speak native QUIC and keep fingerprint pinning
unchanged. See open question below.

### Reachability: make the agent reachable as a server

Full ICE (RFC 8445) requires an ICE agent on *both* peers. The browser side
cannot participate: `WebTransport` is strictly client→server HTTP/3 — no
candidate API, no connectivity checks, no control over its own source port.
Only `RTCPeerConnection` embeds an ICE agent.

This reframes the problem usefully. Seyd does not need symmetric peer
connectivity; it needs **the agent to be reachable as a server**, because the
pilot is always the initiator and its own network is therefore (almost) never
an obstacle. In descending order of reliability:

1. **Router port mapping** (PCP / NAT-PMP / UPnP-IGD) — an installed mapping,
   not an inferred one. Also covers port-restricted NAT.
2. **IPv6** — no NAT in the path, but see the firewall finding below.
3. **STUN reflexive address + hole punching** — full-cone and
   address-restricted NAT only.
4. **Manual port forwarding** of UDP 4433.
5. **Relay** — the future paid tier, for networks where none of the above exist.

The agent gathers all of these at startup into prioritized candidates
(`host` 240, `portmap` 220, `host6` 200, `srflx` 150), classifies its NAT from
two STUN operators, suppresses provably useless candidates (symmetric NAT's
reflexive address, CGNAT-space "public" mappings), publishes a structured
`NatReport`, and hole-punches toward the pilot's address when a session is
announced. The pilot races the candidates (probe-dependent ones held ~400 ms),
first handshake wins, all losers are closed.

**What the field taught us (runs A/B, docs/field-test.md):**

- **The friendly case works as designed.** A robot on an ordinary cone-NAT
  broadband LAN is reachable from anywhere via `srflx` + hole punch — measured
  from a mobile-network pilot at 17–25 ms median glass-to-glass.
- **Chrome's Local Network Access policy** blocks the `host` candidate when a
  page on a public HTTPS origin dials a private IP, until the user grants the
  permission prompt. A same-LAN pilot that declines still connects via the
  router hairpin (`srflx`), at ~12 ms instead of <1 ms; the pilot UI explains
  the trade.
- **Consumer 4G/5G routers block unsolicited inbound on IPv4 *and* IPv6.**
  Verified by probing from a cloud vantage point: outbound-then-reply flows
  pass, unsolicited inbound is dropped by the router's stateful firewall —
  IPv6 removes the NAT, not the firewall, and the pilot's QUIC Initial is by
  definition unsolicited. Out of the box, such a robot is unreachable on
  either address family. **Enabling UPnP/NAT-PMP on the router fixes it
  automatically**: Seyd installs its own pinhole and the session goes direct
  (`portmap`) — proven end-to-end over a double-cellular path at 31 ms median
  glass-to-glass. Failing that: a one-time UDP-4433 forward, a public-IP SIM,
  or the relay tier.
- **Starlink** (docs/starlink.md): every tier gets a globally routable IPv6
  /56, but IPv4 is CGNAT (public IPv4 is a Priority/Business add-on) and *no
  Starlink-issued router on any tier* offers port forwarding or an IPv6
  pinhole. A Seyd robot on Starlink is directly reachable via the official
  Bypass Mode plus the customer's own router opening inbound UDP 4433 over
  IPv6 (any plan; needs an IPv6-capable pilot), or via the public-IPv4 add-on
  (any pilot). Both ends on different Starlinks reduces to the robot-side
  question — the browser pilot is client-only and never needs inbound.
- **Port-restricted and symmetric NAT remain unreachable by construction**
  while the pilot is a browser: they need the browser's ephemeral source port
  (unknowable) or a stable mapping (refused). STUN cannot distinguish
  port-restricted from address-restricted without a cooperating peer, so the
  advertised hint can be optimistic; the definitive answer requires measuring.

**Planned: the cloud prober.** The reachability question that decides
everything — *does an unsolicited packet reach the robot's QUIC port?* — will
be measured from the cloud at announce time (`inbound_ok` per candidate) and
surfaced at enrolment and on the fleet page, so a customer learns "p2p: none —
enable UPnP or forward UDP 4433" before the first pilot ever clicks connect.
The field diagnosis above was performed by hand exactly this way.

**The alternative considered and deferred:** an unreliable, unordered WebRTC
DataChannel would deliver real ICE and TURN, and the jitter-buffer and
transcode objections apply to media tracks, not data channels. The costs are
SCTP/DTLS overhead, a second transport, and an ICE stack in the agent. The
native `seyd/2` path offers a cleaner escape for non-browser pilots
(direction-agnostic QUIC); revisit DataChannel only if field data shows
browser pilots stuck behind unreachable robots that a pinhole cannot fix.

#### SRT: Not a primary candidate
SRT (Secure Reliable Transport, Haivision) is actively maintained (v1.5.6, July 2026; SRT Alliance with AWS, Google, Cloudflare, Microsoft) and excellent for broadcast contribution — but wrong for interactive teleoperation:
- Latency floor ~500ms–1s (ARQ retransmission by design; tunable but reliability degrades)
- No application-level bidirectional data channel (can't carry commands alongside video)
- No browser support (no WASM port exists)
- NAT traversal is weak (rendezvous mode requires known public IP upfront; no STUN equivalent)

**SRT potential role:** Optional archival/recording path — pipe vehicle video to an SRT endpoint for high-quality session logging. Not on the control path.

### Loss Resilience: FEC first, one round trip for the rest

Unreliable datagrams plus whole-frame reassembly means a single lost chunk
destroys a frame, and H.264 delta frames reference their predecessors, so one
loss smears until the next recovery point. Retransmission is the obvious fix
and the wrong one — a retransmit costs a round trip, the one thing
teleoperation cannot spend. Seyd layers three mechanisms, each field-tuned:

1. **Reed-Solomon FEC per block** (`seyd-fec`: GF(256), Cauchy generator —
   every square submatrix invertible, so *any* k losses per block recover).
   Parity is computed per 8-chunk block, so recovery is eager (the moment
   enough chunks of a block exist) and sub-frame pipelining is preserved.
   Prototype-measured and still valid: **99.3 % of frames delivered at 5 %
   independent chunk loss** (71.9 % without). Rust, TypeScript and the Python
   reference are held byte-identical by 55 interop vectors in CI.
2. **Adaptive close-out.** The pilot's frame deadline measures *silence*, not
   elapsed time, and adapts to observed intra-frame chunk gaps (bounded at
   250 ms; a newer decodable frame still closes older ones immediately, so
   this adds no latency on a clean link). Field lesson: LAN-tuned fixed
   deadlines turned cellular jitter into fake loss and keyframe storms.
   Timed-out-but-arrived-late frames are counted separately from genuine loss
   and never trigger recovery.
3. **NACK-driven recovery.** A frame that FEC cannot rebuild is reported
   immediately over the control stream; the agent asks the publisher for a
   recovery point (`recovery-request`, rate-limited to one per 250 ms per
   channel). On the demo camera that is an on-demand IDR; the specified
   preference order for capable encoders is LTR / reference-picture selection,
   then intra-refresh, then IDR (planned — this is also the fix for the
   keyframe-dominated p95 tail).

### Quality of Service: profiles as ceilings, a closed loop inside them

The link constrains *total bytes on the wire*, not video bitrate. A QoS
profile (`latency`, `balanced`, `quality`) is one budget split three ways —
pixels, redundancy, headroom — and states only transport-observable targets:
bitrate ceiling, latency budget, max GOP, plus Seyd's own policy (FEC floors,
drop thresholds, close-out deadlines). **The robot's publisher owns how to
meet the targets** — resolution, preset, VBV — because only it knows its
sensor. Seyd never specifies a resolution.

Within the profile ceiling, a **closed-loop controller**
(`seyd-qos::AbrController`, 1 Hz, pure and trace-tested) adapts to the
measured link:

- **True loss, measured without skew:** the agent pairs the pilot's cumulative
  received-chunk count against what it had sent at least one RTT earlier, over
  a sliding 3–5 s window — a naive same-second comparison misreads an
  in-flight keyframe burst as 10–15 % loss.
- **FEC follows measured loss** (25 % at ≥0.2 %, 38 % at ≥2 %, 50 % at ≥5 %;
  keyframes +10), stepping up immediately and down only after ten clean
  seconds; **parity is paid out of the video bitrate**, so the on-wire total
  never exceeds the ceiling — raising FEC on a saturated link would otherwise
  increase loss.
- **Congestion:** send-backlog drops are the primary signal (−25 % at once);
  the latency rule uses a median RTT over consecutive seconds and ignores
  samples right after a keyframe (a 45 KB IDR on a 2 Mbps uplink *is* 200 ms
  of queue — reacting to it is chasing your own tail). Recovery is +10 % after
  clean seconds; at the bitrate floor the publisher is offered a lower frame
  rate (`suggestedFps`).
- **Hysteresis:** at most one bitrate decision per 5 s and only for ≥10 %
  moves. Field-verified: zero decisions on a clean link, two decisions then
  stable under steady 3 % injected loss, stable near the ceiling in the field.
- Decisions reach the publisher as the same `video-config` message the profile
  does, with `reason: abr-down | abr-up` — on the demo, the bridge applies the
  bitrate cap to the camera live over its own API.

### Video Codec
- **H.264**: use now. Hardware encoders on every ARM SoC; Baseline decode everywhere; the demo runs Baseline 720p25.
- **AV1**: track it. Better compression at low bitrate, but embedded hardware encode is not yet universal (2026). The channel `codec` declaration is where it slots in.
- **H.265/HEVC**: licensing complexity; skip unless a specific customer demands it.

### Signaling
- WebSocket over TLS to the Seyd cloud; JSON messages, versioned (`v: 2`) — docs/protocol/signal-v2.md.
- Robot side: challenge → Ed25519-signed auth → candidate/NatReport announcement → 5 s heartbeats; reconnects with backoff and re-announces. Established P2P sessions survive signaling outages by design.
- Pilot side: connect → offer (candidates, fingerprints, channels, role, NatReport) → race → accepted/report. Failed attempts release their slot immediately.

### Fleet Registry (planned)
- Vehicle identity: the Ed25519 key pair, enrolled via one-time tokens.
- Registry: Postgres (orgs, users, robots, grants, sessions with outcome/path/failure-reason metrics); Redis for presence and cross-instance routing. Today presence is in-memory on a single instance — a deploy briefly blips it, a known cost until the Redis store lands.

---

## Competitive Landscape

### Voysys / Oden (acquired by Serve Robotics, September 2025)
Previously a standalone teleoperation platform (Swedish origin, acquired by Phantom Auto). Built "Oden" — a purpose-built proprietary multi-link transport stack, not WebRTC. Claims <45ms glass-to-glass over cellular. Handles 4G/5G/WiFi bonding with custom FEC, adaptive bitrate, and careful modem-buffer management. Ran 2,000+ vehicles daily across 10 industries; vehicle side ran on Jetson AGX Orin at ~2% GPU for 6 × 1080p cameras. **Now fully internalized by Serve Robotics — no longer available as a product.** This is the most direct market gap: the best purpose-built connectivity stack in the space just went off the market.

### Ottopia
Israeli company targeting AV OEMs and defense (Hyundai, Magna, IDF). Full-stack — they own the operator UI. Uses DTLS + SRTP, proprietary AI-enhanced super-resolution, multi-path bonding, cross-channel FEC. Not middleware; not available as an SDK. Defense pivot reduces overlap with commercial robotics. Still active, Series A funded.

### Adamo (founded 2025; San Francisco + London)
Most directly comparable in positioning — hardware-agnostic, native ROS/ROS2 support, claims sub-40ms latency. Their framing explicitly calls out WebRTC as too slow ("WebRTC was built for video calls, not controlling robots"). Uses a custom transport with multi-path bonding over LTE/5G/Wi-Fi.

Re-checked from adamohq.com on 2026-09-02; two earlier statements here were wrong:

- **Pricing is public.** Streaming platform **$50/robot/month** (50 teleop hours included, **$0.90/hour** overage, "dedicated bandwidth", 99% uptime SLA); managed operators **from $12/operator-hour**; enterprise custom. The platform is sold **standalone** — this is not operators-only bundling, so they compete directly for the middleware sale.
- **Product surface is ahead of ours.** SDKs for Python, Rust, C and TypeScript; hosted console at `operate.adamohq.com` with gamepad and VR teleop, recording and replay; purpose-built interfaces for humanoids, arms, AVs and AMRs; AES-256, "built to be SOC 2 compliant".

Latency budget from their engineering blog: encode 3–5 ms, network transit 15–25 ms, decode+render 5–8 ms, **total 25–38 ms glass-to-glass**, conditions given only as "robot in a warehouse, operator in another city". No percentile and no tail published. That is the same range as our measured 17–31 ms p50 (§ field results) — **latency is not a differentiator against Adamo**, and neither figure is independently verified.

Their media path is **not disclosed anywhere** on the site, pricing page, FAQ or docs. Two things imply an aggregation point rather than P2P: carrier bonding requires something terminating and reordering the paths, and teleoperation cannot be metered per hour if the bytes never cross the vendor's network. Treat as inference until confirmed. What follows if true is the real axis of competition — our P2P architecture forecloses bonding and recording, and their architecture forecloses zero-marginal-cost pricing, structural privacy and self-hosting.

**No self-hosted or on-premise option** appears anywhere in their material, and no named customer, logo or case study. Our differentiation is architecture, auditability and cost, not speed.

### LiveKit / Portal
LiveKit is open-source WebRTC infrastructure (SFU + signaling, Rust/Go). In 2025 they launched **Portal**, a robotics-specific wrapper: per-tick observation bundling (camera + joint state + timestamp arrive together), Robot/Operator roles in Python with a unified Rust core. Polymath Robotics uses it for remote heavy machinery. **Closest existing building block to what Seyd is** — but WebRTC-only (latency floor ~100–200ms), and Portal is a thin layer, not a purpose-tuned teleoperation stack. Open-source model is a competitive advantage for adoption; also a moat-reduction risk.

### Transitive Robotics
Modular cloud platform for robot operations: live video, remote teleop, deployment, observability. WebRTC for P2P, MQTT for state sync. More of a full robotics-ops SaaS than a connectivity SDK. Niche, small team.

### Viam
Full robotics platform — gRPC for structured RPCs, WebRTC for P2P streaming, cloud-managed fleet. Significant funding (MongoDB founder). They want to own the whole robot software stack, not just connectivity. Not a pure competitor; a different layer.

### Open-Source Reference
- **phntm_bridge**: Fast WebRTC + Socket.io ROS2 bridge (C++). Now open at docs.phntm.io/bridge.
- **LiveKit Portal**: Best production-grade open option today. Rust + Python.
- No complete, purpose-built, drop-in connectivity platform exists in open-source.

### Market Gap Summary
| Player | Stack layer | Transport | Middleware-only? | Available? |
|---|---|---|---|---|
| Voysys/Oden | Connectivity | Custom QUIC-like | Yes | No (internalized) |
| Ottopia | Full stack | Proprietary | No | Enterprise only |
| Adamo | Connectivity + operators | Custom (bonded, path undisclosed) | Yes (platform sold standalone) | Yes — $50/robot/mo |
| LiveKit Portal | Connectivity | WebRTC | Yes | Yes (open-source) |
| Transitive | Full ops platform | WebRTC | Partial | Yes |
| Viam | Full robot platform | WebRTC + gRPC | No | Yes |
| **Seyd** | **Connectivity** | **QUIC / WebTransport** | **Yes** | **Working stack, pre-GA (field-tested demo)** |

---

## Business Model & Licensing

Internal position, tiers and unit economics live in **docs/business.md**.
The one product-shaping consequence belongs here: **the SDKs — everything that
runs on customer hardware, robot and pilot side — are open source
(Apache-2.0)**, with no held-back components; the protocol contracts are
public. What is sold is the coordination plane (hosted and self-hosted
signaling, enterprise features) and, later, the metered relay tier.

## Open Questions

Answered since the prototype era (kept as a short decision log):

- **Codec negotiation, shape of** — answered by the requested-config contract:
  Seyd states bitrate ceiling / latency budget / max GOP / suggested fps with a
  reason; the publisher maps them onto its sensor and encoder. Codec *identity*
  negotiation (H.264 vs AV1, deriving decoder config from the SPS) remains open.
- **ROS 2 form factor** — both: a thin rclcpp node and the standalone daemon
  wrap the same core; neither contains protocol logic (ADR 0004).
- **MoQ** — not adopted for v2; its value is relay fan-out, the deferred tier.
  Its object model maps onto channel/GOP/frame, so an adapter stays possible
  (ADR 0003). Re-evaluate at the relay-tier decision.
- **FEC sizing** — no longer a static per-profile guess: parity follows
  measured loss inside the profile budget (see QoS).
- **Agent language** — Rust on quinn (ADR 0002) superseded the earlier
  "C + MsQuic" position; the relay-fallback design was superseded by the
  P2P-only decision.

Still open:

1. **Pricing numbers** — the tier *structure* is now a position (see Business Model & Licensing); the actual price points per robot/month, the free-tier robot count (1 vs 2), and the relay metering unit (GB vs active-minute) remain open. Field run B showed the relay segment (locked-down cellular/satellite robots) is real.
2. **Multi-path / link bonding** — MPQUIC maturity in the Rust stacks; a later-phase differentiator (Voysys and Adamo both lead with bonding).
3. **Session recording** — synchronized video + sensor + command capture for training data and post-incident review; design the data model early.
4. **Auth provider** — the OIDC abstraction is decided; the provider (self-hosted Zitadel/Keycloak vs managed EU-hosted) is not. Must be settled before the first external customer signs up.
5. **Safari** — verify `serverCertificateHashes` and H.264 WebCodecs behaviour; decide on the CA-signed-cert fallback (`<robot>.p2p.seyd.io` wildcard + DNS-01) if lacking.
6. **Relay-tier design** — a QUIC-forwarding relay with a public address (a browser WebTransport client cannot use TURN proper); geographic placement; pricing (see 1).

---

## Current State (as of 2026-08-31)

The Seyd stack is built, deployed, and field-tested on the always-on Hikvision
PTZ demo (DEMO.md). The Python/JS proof of concept is deleted; PROTOTYPE.md
records it and its still-valid measurements.

**What exists and is verified:**
- Rust workspace: `seyd-wire` (v2 wire format), `seyd-fec` (RS Cauchy,
  interop-vector-locked), `seyd-qos` (profiles + field-tuned ABR controller),
  `seyd-nat` (STUN/classification, PCP/NAT-PMP/UPnP, candidates, NatReport),
  `seyd-transport` (quinn + WebTransport, BBR, probing), `seyd-signal-client`
  (Ed25519 auth, reconnect), `seyd-core` (engine: multi-session fan-out,
  driver/observer enforcement, bounded in-order sender, recovery requests),
  `seydd` (TOML daemon; RTSP via retina and an in-house RFC 6184 RTP
  depacketizer; UDP sensor/command channels; publisher-control interface).
- Web pilot SDK: `@seyd/core` (worker-hosted engine, candidate race,
  per-block FEC recovery, adaptive close-out, clock-synced glass-to-glass,
  duplicate-session guards), `@seyd/web` custom elements with role badge and
  per-class connect-error guidance, `web/demo` pilot page with PTZ controls.
- Cloud (`cloud/api`, signal v2) on Cloud Run `europe-west1`, project
  `seydio`: `https://seyd-signal-flj7s44j4a-ew.a.run.app` — landing page with
  the live robot list at `/`, pilot at `/pilot/`, dev-mode auth (TOFU robot
  enrolment, anonymous pilots), in-memory presence. Deploy:
  `GCLOUD_PROJECT=seydio bash cloud/api/deploy.sh`; `cloud/docker-compose.yml`
  is the portability proof.
- Demo: `./demo-seyd.sh` (robot id `seyd-demo`) — `seydd` plus a vendor bridge
  in `examples/demo-robot/` (ISAPI PTZ with momentary windows and
  park-on-release, keyframe-on-request, live ABR bitrate caps). Verified
  end-to-end by `tools/seyd-smoke.py` (headless Chrome, real decode asserts,
  camera-movement oracle, stats recorder) on LAN, single-cellular and
  double-cellular paths. `./sim-robot.sh` runs a webcam robot for field tests
  without the camera.
- Field results and reachability findings: docs/field-test.md (runs A and B),
  docs/starlink.md.

**Not built yet (ordered work in PLAN.md):**
- Cloud persistence and auth: Postgres/Redis, OIDC login, enrolment and
  session tokens enforced end-to-end, org RBAC, console beyond the fleet list
- The cloud prober (`inbound_ok` at enrolment)
- `seyd-ffi`/`seyd.h`, Python wheels, ROS 2 node, C++ wrapper
- Headless pilot agent and native `seyd/2` (direction-agnostic QUIC)
- LTR/intra-refresh recovery; PMTUD-driven chunk size; cert rotation;
  candidate re-gather on network change; port-mapping lease renewal in-session
- Relay tier; mobile SDKs (on customer demand); session recording; netem CI
  and hardware-in-loop runners

## Next Steps

1. **Cloud prober** — measure `inbound_ok` per candidate at announce time and
   surface it at enrolment and on the fleet page; field run B was this
   measurement done by hand, and it decides the customer conversation.
2. **Make the cloud real** — Postgres/Redis persistence, OIDC login/sign-up,
   enrolment tokens, session tokens verified by the robot; the gate before any
   non-demo robot is exposed.
3. **Agent SDK surface** — `seyd-ffi` + `seyd.h`, Python wheels, ROS 2 node,
   so integration stops requiring the daemon form.
4. **Recovery quality** — LTR/intra-refresh preference order to kill the
   keyframe-dominated p95 tail measured in the field.
5. **Long-lived-robot robustness** — network-change re-gather, port-mapping
   renewal, cert rotation.
6. **Relay tier design** — the priced tier for the locked-down-network segment
   that field runs A/B established is real.
