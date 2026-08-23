# DARC — Distributed Autonomous Remote Control

## Product Vision

DARC is a software-as-a-service platform that enables low-latency, secure, peer-to-peer remote operation of autonomous vehicles and robots over the internet. DARC is not the robot, the pilot system, or the control UI — it is the connectivity layer: a set of drop-in software components that any robot or vehicle system can integrate to gain remote-operation capability.

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

Understanding who integrates DARC shapes every product decision.

### Archetype A — "I have a robot, I need a pilot app"
Building from scratch or operating a simple system. Wants DARC to handle as much as possible: video delivery to a browser, embeddable UI components, sensor widgets. Will struggle with codecs, WebRTC negotiation, and mobile integration if left to figure it out alone. **These customers want DARC to be opinionated.**

### Archetype B — "I have a pilot system, I need it to work over the internet"
Already has a desktop pilot application. It reads UDP from a LAN port and renders video natively. Their robot already publishes RTP or a proprietary stream. They don't want DARC to touch their pipeline — they want DARC to make `robot-lan:5000` appear as `localhost:5000` on the pilot machine, securely, over the internet, with low latency. **These customers want DARC to be invisible.**

The mental model for Archetype B: **DARC is a VPN for robot LAN data.** The pilot application never knows it isn't on the same LAN as the robot. DARC creates virtual UDP sockets on the pilot machine; the application reads from `localhost:5000` as always.

Both archetypes ride the same DARC transport layer. The difference is how much of the stack above the transport DARC owns.

---

## What DARC Is (and Is Not)

| DARC provides | DARC does not provide |
|---|---|
| Peer-to-peer video tunnel | The camera or video encoder |
| Sensor data channel | The sensors or sensor fusion |
| Command channel | The control algorithms or autopilot |
| Vehicle discovery & fleet registry | The vehicle hardware |
| Session authentication & authorization | The operator UI (except optional SDK components) |
| Fleet presence & status signaling | The fleet management business logic |
| TURN relay fallback | Managed human operators |
| Optional pilot UI components (React, SwiftUI) | Transcoding or format conversion |

DARC exposes SDKs and APIs. Integrators build the robot agent and the operator application on top.

---

## Core Use Cases

### 1. Teleoperation (Direct Control)
An operator takes full manual control of a vehicle in real time. Latency is critical — commands and video must be synchronized to avoid disorientation and accidents.

- **Example:** A delivery robot is stuck; a remote operator steers it around the obstacle.
- **Latency target:** <100ms end-to-end (achievable on continental connections; ~150–200ms intercontinental is realistic physics-limited floor)

### 2. Supervisory Control
An autonomous vehicle operates on its own but streams video and sensor data to an operator dashboard. The operator monitors and can intervene at any time.

- **Example:** An autonomous truck drives a highway route; a dispatcher watches 10 vehicles simultaneously.
- **Latency target:** <200ms acceptable; operator is not actively steering

### 3. Assisted Takeover (Vehicle-Initiated)
The vehicle's onboard system detects a situation it cannot handle and signals the operations center. An available operator is dispatched to the session.

- **Example:** A robot flags "low confidence" in its perception stack; the platform alerts an operator.
- **Key requirement:** Vehicle-side "help request" API; operator assignment/queue system

### 4. Fleet Monitoring
An operator views a dashboard of all active vehicles — location, health, stream thumbnails — without necessarily being connected to any single vehicle in detail.

- **Key requirement:** Lightweight presence/telemetry channel (not full video for all vehicles simultaneously)

### 5. Multi-Vehicle Takeover
A single operator is connected to more than one vehicle at once, switching focus between them.

- **Key requirement:** Session multiplexing; operator can have N sessions open simultaneously

---

## Architecture Overview

```
┌─────────────────────────────────────────┐
│            DARC Cloud                   │
│  ┌──────────┐  ┌──────────────────────┐ │
│  │ Signaling│  │  Fleet Registry &    │ │
│  │ Server   │  │  Presence Service    │ │
│  └──────────┘  └──────────────────────┘ │
│  ┌──────────────────────────────────────┤
│  │   TURN Relay (fallback only)         │
│  └──────────────────────────────────────┤
└─────────────────────────────────────────┘
         │  signaling only          │
         │  (WebSocket/HTTPS)       │
    ┌────▼─────┐              ┌─────▼──────┐
    │  DARC    │◄────────────►│   DARC     │
    │  Agent   │  P2P tunnel  │  Operator  │
    │ (on robot│  (QUIC /     │  SDK       │
    │  or LAN  │   WebRTC)    │            │
    │  gateway)│              └────────────┘
    └──────────┘                    │
         │                   Web / Desktop /
    Robot hardware             Mobile app
    Sensors, camera
```

### Components

#### DARC Agent (vehicle-side SDK)
- Runs on the robot, vehicle edge computer, or a LAN gateway co-located with the vehicle
- Captures and encodes video (hooks into existing camera pipeline)
- Publishes sensor telemetry
- Receives and forwards operator commands
- Manages P2P connection lifecycle
- Available as: Linux daemon, ROS 2 node, Docker container, C++ / Python library

#### DARC Operator SDK
- Used to build the operator-facing application
- Manages P2P connection to a vehicle
- Provides decoded video stream
- Provides sensor data stream
- Sends command messages
- Available as: TypeScript/JS (browser + Electron), Swift (iOS/macOS), Kotlin (Android)

#### DARC Cloud (signaling & registry)
- **Signaling server:** Brokers connection setup without touching media
- **Fleet registry:** Persistent record of vehicles, their owners, and configuration
- **Presence service:** Real-time vehicle online/offline/busy/help-requested status
- **Auth service:** Issues short-lived session tokens; enforces who can connect to what
- **TURN relay:** Fallback for environments where direct P2P is blocked by NAT/firewall (media touches cloud only here)

---

## Data Channels

### Video Channel
- Direction: Vehicle → Operator (unidirectional)
- Codec: H.264 (hardware-encodable on ARM today; AV1 when hardware encoders are common)
- Target latency: <80ms encode+transmit+decode on LAN; <200ms intercontinental (physics-limited)
- Adaptive bitrate based on available bandwidth; per-link multi-path bonding optional

### Sensor/Telemetry Channel
- Direction: Vehicle → Operator (unidirectional, continuous)
- Content: GPS position, speed, heading, battery, custom sensor payloads
- Protocol: lightweight binary (Protobuf over QUIC stream or WebRTC data channel)
- Frequency: configurable per sensor type (1–100 Hz)

### Command Channel
- Direction: Operator → Vehicle
- Content: control inputs (steering, throttle, brake, custom commands)
- Reliability: ordered, low-latency; unreliable/ordered mode for continuous gamepad inputs; reliable for discrete commands
- Authenticated: each session carries a short-lived token; vehicle validates before accepting any command

### Presence/Signaling Channel
- Direction: Bidirectional (vehicle ↔ cloud)
- Content: heartbeat, status updates, help requests, session negotiation
- Protocol: WebSocket over TLS to DARC Cloud

---

## Security Model

- All P2P media and data are encrypted end-to-end (DTLS-SRTP for WebRTC, TLS 1.3 for QUIC)
- The DARC Cloud never sees unencrypted media — it brokers connection setup only
- Vehicles are provisioned with a unique identity (asymmetric key pair)
- Operators authenticate via DARC Cloud (OAuth2 / API key); receive short-lived session tokens
- Vehicles verify operator session tokens before accepting any commands
- Fleet owners control which operators can access which vehicles (RBAC)

---

## Latency Reality

Understanding the true latency budget is critical for product positioning:

**Glass-to-glass breakdown (WebRTC baseline):**
| Component | Typical |
|---|---|
| Camera capture + USB | ~100ms (biggest single factor; camera choice matters) |
| H.264 encode | ~10ms |
| H.264 decode | ~10ms |
| WebRTC stack overhead | ~10ms (negligible) |
| Jitter buffer | 50–100ms (tunable — the main software lever) |
| Network (continental) | 20–40ms |
| Network (intercontinental) | 80–150ms (speed of light floor: ~40ms Atlantic, ~65ms Pacific) |

**Realistic targets:**
- LAN / same city: 50–100ms achievable
- Continental (US or EU): 100–150ms achievable with WebRTC; 70–120ms with QUIC
- Intercontinental: 150–250ms — physics-limited, no protocol choice changes this materially

The jitter buffer (50–100ms) is the largest software-controllable variable. Custom QUIC transport eliminates the WebRTC jitter buffer entirely; this is the primary latency argument for QUIC over WebRTC.

**Cellular networks:** WebRTC interprets packet loss as congestion and backs off. Cellular loss is transient (handovers), not congestion — backing off is wrong. QUIC with per-stream loss isolation handles this correctly. This is a material difference for mobile-connected vehicles.

---

## Technology Choices

### Video Format Principle: No Transcoding in DARC

The DARC Agent is a pure relay — it forwards H.264 packets byte-for-byte without decoding, re-encoding, or inspecting the payload. DARC never transcodes.

The responsibility for codec compatibility sits with the robot-side publisher. H.264 Baseline is universally supported in every browser, every OS, and every mobile platform. If the robot publishes H.264 Baseline, any pilot — browser, native desktop, mobile — works with zero transcoding. This is the recommended default.

For robots that output an incompatible format (H.265, raw YUV, proprietary codec): DARC provides an optional **transcoding sidecar** — a standalone Docker container that a customer runs on their own infrastructure between the robot and the DARC Agent. DARC itself never touches encoding. This keeps DARC's latency guarantees intact and eliminates cloud transcoding cost.

### Pilot-Side Integration

**For Archetype A** (building a pilot app from scratch): DARC provides first-party UI components:
- React `<DarcVideoPlayer sessionId={...} />` — handles WebRTC/QUIC internally
- SwiftUI `DarcVideoView` — for iOS/macOS native apps
- Without these, every customer reinvents the same WebRTC plumbing, badly

**For Archetype B** (existing pilot system): DARC Operator SDK creates virtual UDP sockets on the pilot machine. The existing pilot application reads from `localhost:5000` as if the robot were on the same LAN. DARC is invisible to the application.

Both delivery mechanisms ride the same underlying transport.

### Why WebRTC Is Wrong for Teleoperation

WebRTC was designed for video calls, where smooth playback is more important than minimum latency. For teleoperation, this is exactly backward.

**The jitter buffer problem.** WebRTC's browser jitter buffer delays frame delivery to absorb network jitter. It is controlled by the browser, not the application — you can hint at a target but cannot zero it out. The effective floor is 50–200ms. For a teleoperation control loop, this is catastrophic: an operator sees the world as it was 50–200ms ago, then sends a command, then waits for the vehicle to respond. The feedback loop degrades.

**The double encode/decode problem.** An aiortc-based robot agent must decode the incoming H.264 RTP stream (via PyAV/libav) to attach it to a WebRTC track — which then re-encodes it for delivery. Two full encode/decode cycles before the browser's own decode. Each cycle adds ~10ms and degrades image quality.

**Latency comparison (LAN):**

| Stage | WebRTC + aiortc | WebSocket + WebCodecs |
|---|---|---|
| H.264 encode (FFmpeg) | ~10ms | ~10ms |
| RTP → PyAV decode | ~10ms | eliminated |
| WebRTC re-encode | ~10ms | eliminated |
| Jitter buffer | 50–200ms | 0ms |
| H.264 decode (WebCodecs) | ~5ms | ~5ms |
| **Total (software)** | **~85–235ms** | **~15ms** |

WebCodecs replaces the jitter buffer with a call to `decoder.decode()` — frames are rendered as soon as they arrive. The single encode chain is: FFmpeg H.264 encode → passthrough → WebCodecs decode. Nothing in between.

### Transport Protocol: Two-Phase Approach

#### Phase 1 — POC: WebSocket + WebCodecs

The prototype validates the single-encode-chain concept using WebSocket as the transport:

- **Video path:** FFmpeg H.264 RTP → PyAV demux (NAL unit extraction) → binary WebSocket frame → signal server relay → pilot WebSocket → WebCodecs VideoDecoder → canvas. No jitter buffer. No re-encode.
- **Data path:** JSON text WebSocket messages for sensor data and commands, relayed through the signal server.
- **NAT traversal:** None in the prototype — video is relayed through the signaling WebSocket server, not peer-to-peer. This is an explicit POC compromise.
- **Codec:** H.264 Baseline, forwarded byte-for-byte. WebCodecs `VideoDecoder` configured with `avc1.42001f` (Baseline Level 3.1), `optimizeForLatency: true`.

This removes aiortc and WebRTC entirely from the video path. The prototype proves that WebCodecs decode works, measures the latency improvement, and establishes the architectural pattern that production will use.

#### Phase 2 — Production: QUIC + WebTransport

**Status: partially implemented in the prototype** (see PROTOTYPE.md for full detail).

The Mac-to-Mac prototype now runs this transport architecture end-to-end, using Python/aioquic as a stand-in for the production MsQuic agent. The key design decisions have been validated on real hardware including a cellular (iPhone 5G hotspot) path.

**What is implemented in the prototype:**
- WebTransport P2P: aioquic server on the robot, Chrome WebTransport on the pilot
- QUIC DATAGRAM frames for video (unreliable, no HOL blocking, no jitter buffer)
- Application-layer fragmentation: H.264 access units split into ≤1000-byte chunks with a 7-byte header (frame_id, chunk_idx, total_chunks, keyframe flag) to fit QUIC DATAGRAM MTU
- Multi-candidate connection: robot advertises host (LAN) IPs + STUN-reflexive address; pilot races all in parallel; first to connect wins
- Self-signed ECDSA P-256 TLS with `serverCertificateHashes` API — no CA chain, fingerprint pinned via signal server
- NAT hole-punching: robot probes pilot IP from the WebTransport UDP socket before pilot's QUIC Initial arrives
- Latest-frame-only sender: single-slot frame buffer + aioquic datagram queue flush before each frame — no latency growth on congested paths
- WebSocket relay fallback: automatic after 10s P2P timeout; same chunk format; pilot detects ArrayBuffer on WebSocket and routes through the same decoder path

**What the production version adds:**
- Native QUIC via **MsQuic** (C library, Microsoft) on the agent — Python/aioquic replaced for ARM embedded targets
- Standard ICE/TURN for robust NAT traversal (currently: STUN + relay fallback via signal server WebSocket)
- WebRTC fallback for environments where UDP is blocked entirely
- Desktop and iOS operator SDKs (currently browser-only)

**Browser operator:** **WebTransport** (QUIC semantics in the browser). WebTransport reached Baseline status in March 2026 with Safari 26.4 shipping support — it is now safe to depend on in all major browsers. Same WebCodecs VideoDecoder as in the POC; only the transport changes.

**Signaling path is shared:** The DARC Cloud signaling server is identical in both phases. Switching transport is isolated to the SDK layer.

#### SRT: Not a primary candidate
SRT (Secure Reliable Transport, Haivision) is actively maintained (v1.5.6, July 2026; SRT Alliance with AWS, Google, Cloudflare, Microsoft) and excellent for broadcast contribution — but wrong for interactive teleoperation:
- Latency floor ~500ms–1s (ARQ retransmission by design; tunable but reliability degrades)
- No application-level bidirectional data channel (can't carry commands alongside video)
- No browser support (no WASM port exists)
- NAT traversal is weak (rendezvous mode requires known public IP upfront; no STUN equivalent)

**SRT potential role:** Optional archival/recording path — pipe vehicle video to an SRT endpoint for high-quality session logging. Not on the control path.

### Video Codec
- **H.264**: Use now. Hardware encoders on every ARM SoC; GStreamer pipeline is mature.
- **AV1**: Track it. Better compression at low bitrate, but hardware encode on embedded is not yet universal (2026). Plan to support it in the codec negotiation API so integrators can opt in.
- **H.265/HEVC**: Licensing complexity; skip unless a specific customer demands it.

### Signaling
- **WebSocket over TLS** to DARC Cloud: simple, reliable, works everywhere, easy to implement heartbeat and reconnect.
- Consider upgrading to **gRPC streaming** later for structured messages and generated client stubs.

### Fleet Registry
- Vehicle identity: asymmetric key pair provisioned at manufacturing/setup time
- Registry: standard relational DB (Postgres) with a real-time events layer (Redis pub/sub or similar)
- Presence: separate lightweight service; vehicles heartbeat every 5s; status is ephemeral

### TURN Relay
- Self-host **coturn** initially; evaluate **Cloudflare TURN** for managed global edge coverage at scale.
- TURN should be a fallback, not the default — track the % of sessions that fall back to relay as a product health metric.

---

## Competitive Landscape

### Voysys / Oden (acquired by Serve Robotics, September 2025)
Previously a standalone teleoperation platform (Swedish origin, acquired by Phantom Auto). Built "Oden" — a purpose-built proprietary multi-link transport stack, not WebRTC. Claims <45ms glass-to-glass over cellular. Handles 4G/5G/WiFi bonding with custom FEC, adaptive bitrate, and careful modem-buffer management. Ran 2,000+ vehicles daily across 10 industries; vehicle side ran on Jetson AGX Orin at ~2% GPU for 6 × 1080p cameras. **Now fully internalized by Serve Robotics — no longer available as a product.** This is the most direct market gap: the best purpose-built connectivity stack in the space just went off the market.

### Ottopia
Israeli company targeting AV OEMs and defense (Hyundai, Magna, IDF). Full-stack — they own the operator UI. Uses DTLS + SRTP, proprietary AI-enhanced super-resolution, multi-path bonding, cross-channel FEC. Not middleware; not available as an SDK. Defense pivot reduces overlap with commercial robotics. Still active, Series A funded.

### Adamo (emerged from stealth 2025)
Most directly comparable in positioning — hardware-agnostic, native ROS/ROS2 support, claims sub-40ms latency. Key difference: they also sell managed human operators alongside the software. Their framing explicitly calls out WebRTC as too slow. Uses custom multi-path bonding stack. Small and early-stage; pricing not public.

### LiveKit / Portal
LiveKit is open-source WebRTC infrastructure (SFU + signaling, Rust/Go). In 2025 they launched **Portal**, a robotics-specific wrapper: per-tick observation bundling (camera + joint state + timestamp arrive together), Robot/Operator roles in Python with a unified Rust core. Polymath Robotics uses it for remote heavy machinery. **Closest existing building block to what DARC would be** — but WebRTC-only (latency floor ~100–200ms), and Portal is a thin layer, not a purpose-tuned teleoperation stack. Open-source model is a competitive advantage for adoption; also a moat-reduction risk.

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
| Adamo | Full stack + operators | Custom | No | Early stage |
| LiveKit Portal | Connectivity | WebRTC | Yes | Yes (open-source) |
| Transitive | Full ops platform | WebRTC | Partial | Yes |
| Viam | Full robot platform | WebRTC + gRPC | No | Yes |
| **DARC** | **Connectivity** | **QUIC + WebRTC** | **Yes** | **To build** |

---

## Open Questions (Remaining)

1. **Multi-path / link bonding** — Voysys and Adamo both emphasize 4G+5G+WiFi bonding as a key differentiator. QUIC's multi-path extension (RFC 9000 + MPQUIC draft) could give us this without a custom stack. How mature is MPQUIC in MsQuic? This could be a phase 3 feature.
2. **MoQ timing** — Media over QUIC (MoQ, draft-17) is being productized by Cloudflare and 11 vendors demonstrated interoperability at NAB 2026. Should we track MoQ as the video transport spec, or build our own RTP-over-QUIC framing?
3. **Pricing model** — per-vehicle-per-month (predictable for customers), per-minute of active session (scales with usage), or bandwidth-based (hard to predict). Voysys used per-vehicle pricing.
4. **ROS2 agent form factor** — should the DARC agent be a native ROS2 node (tight integration, requires ROS2) or a standalone daemon with a ROS2 bridge adapter (wider compatibility, two processes)?
5. **Session recording** — built-in synchronized recording of video + sensor data + commands is valuable for training data and post-incident review. Design the data model early.
6. **Codec negotiation** — how does the vehicle agent and operator SDK negotiate codec, resolution, and bitrate? Define this API surface before it ossifies.

---

## Current State (as of 2026-08-22)

The Mac-to-Mac prototype is complete and working. See PROTOTYPE.md for full implementation detail.

**What exists:**
- `darc-signal`: Node.js WebSocket server on Cloud Run, handles registration, connection brokering, binary relay fallback, fleet UI
- `darc-agent`: Python daemon with WebTransport P2P (aioquic), STUN/hole-punching, automatic relay fallback via signaling WebSocket
- `darc-pilot`: Vanilla JS browser app with WebTransport candidate racing, chunk reassembly, WebCodecs decode, relay fallback
- End-to-end tested: LAN, broadband-to-broadband, broadband-to-cellular (relay fallback)

**What is not built yet:**
- Auth, fleet registry, RBAC
- TURN relay (current relay uses WebSocket through signal server — prototype-quality only)
- MsQuic-based C agent for ARM embedded targets
- Operator SDKs for desktop and mobile
- ROS2 node wrapper
- Multi-camera support
- Session recording

## Next Steps

### Short-term (prototype polish)
1. **Relay-mode commands** — route pilot→robot JSON commands through the signaling WebSocket in relay mode (currently commands silently no-op when P2P fails)
2. **Latency display** — embed a wall-clock timestamp in the chunk header; display glass-to-glass latency on the pilot page to confirm QUIC path advantage over relay
3. **Port agent to C with MsQuic** — Python/aioquic is prototype-quality; the production agent needs to run on ARM Linux (Jetson, RPi 5, RK3588) with MsQuic for sub-10ms QUIC processing overhead

### Medium-term (path to product)
4. **Standard TURN relay** — replace WebSocket relay fallback with coturn or Cloudflare TURN; implement ICE candidate exchange for proper NAT traversal across all NAT types
5. **Agent API surface** — define the integration contract: configuration, lifecycle, stream hooks, command callbacks. This determines what ROS2 node or Linux daemon integrators would call.
6. **Operator SDK API surface** — TypeScript first (browser + Electron); what does a 10-line integration look like?
7. **Fleet registry data model** — vehicle identity, owner, operator RBAC, presence events; Postgres + Redis pub/sub

### Architecture decisions pending
8. **MoQ vs custom framing** — Media over QUIC (MoQ, draft-17) demonstrated interoperability at NAB 2026. Evaluate whether adopting MoQ as the video transport spec is better than the current custom chunk format.
9. **MPQUIC** — multi-path QUIC (RFC 9000 + MPQUIC draft) in MsQuic could give link bonding (4G+5G+WiFi) without a custom stack. Evaluate maturity.
10. **ROS2 agent form factor** — native ROS2 node vs standalone daemon with ROS2 bridge adapter.
