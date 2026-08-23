# DARC Prototype — Mac-to-Mac Teleoperation

## Current Status (as of 2026-08-22)

**Phase 2 is implemented and working.** The prototype now runs the production transport architecture — WebTransport over QUIC (P2P) as the primary path, with automatic fallback to WebSocket relay through the signal server when P2P cannot be established. Both paths are tested on real hardware.

| Scenario | Works | Notes |
|---|---|---|
| Robot + pilot on same LAN | ✓ | Host candidate wins immediately, fired without the probe hold |
| Robot on broadband, pilot on broadband | ✓ | Port mapping where the router allows it, otherwise STUN + hole punching |
| Robot behind UPnP/NAT-PMP/PCP router | ✓ | Explicit port mapping — also covers port-restricted NAT |
| Robot and pilot both on IPv6 | ✓ | No NAT in the path at all |
| Robot on iPhone hotspot (CGNAT), pilot on broadband | ✓ (relay) | Symmetric NAT detected at startup; fails over in ~2s instead of 10 |
| Relay → P2P upgrade | ✓ | Retried every 30s while relaying; switches over live when it succeeds |
| Latency on good path | ✓ | Perceptibly lower than WebRTC; no jitter buffer |
| Latency on congested/mobile path | ✓ | Agent always sends latest frame; no queue buildup |

---

## Goal

Validate the DARC single-encode-chain architecture end-to-end on real hardware and over a real cellular path. One Mac acts as the robot, one Mac acts as the pilot station. The prototype proves that:

1. H.264 can be forwarded byte-for-byte — no transcoding, no jitter buffer
2. WebCodecs VideoDecoder latency is materially lower than WebRTC
3. WebTransport P2P works over the public internet for reachable robots
4. Relay fallback works automatically for robots behind symmetric carrier NAT

---

## Design Principles

**Loose coupling.** Each component communicates only through defined interfaces: UDP sockets, WebSocket messages, and WebTransport QUIC datagrams. No component imports another's internals.

**High cohesion.** Each component does one thing. The agent relays; the signal server routes; the pilot renders.

**Production-boundary awareness.** Components in `packages/` are production-worthy in structure. Components in `sim/` simulate robot hardware and are never imported by `packages/`.

**No jitter buffer.** Frames are rendered as they arrive. The agent's `_frame_sender` holds only the latest decoded frame in a single slot — when a new frame arrives before the previous one finishes sending, the old one is abandoned. This prevents queue buildup regardless of network conditions.

**No transcoding.** The agent reads H.264 packets from PyAV and forwards them byte-for-byte. It never decodes video.

**Dual-path transport.** Primary: WebTransport P2P (QUIC datagrams). Fallback: WebSocket relay via signal server (same chunk format, same pilot decoder). The pilot can't tell which path is active once video starts.

---

## Architecture

```
Robot Mac                                           Pilot Mac (browser)
─────────────────────────────────                   ──────────────────────

── Robot simulation (prototype-only, sim/) ──

┌───────────────────────┐
│ ffmpeg                │ (webcam → H.264 RTP → UDP 127.0.0.1:5000)
└────────┬──────────────┘
┌────────┴──────────────┐
│ sensor-source.py      │ (counter → UDP 127.0.0.1:5002 at 10 Hz)
└────────┬──────────────┘

── DARC components (packages/) ──

         │ UDP (localhost)
┌────────▼──────────────┐
│  darc-agent           │
│  ─────────────────    │
│  PyAV demux RTP       │
│  → H.264 Annex B      │──── QUIC datagrams (P2P) ──────────────────►┐
│                       │                                              │
│  Latest-frame sender  │──── binary WS frames (relay fallback) ─────►│
│  (single-slot, no     │                                              │
│  queue buildup)       │◄─── JSON commands (via bidi stream / WS) ───┤
│                       │                                              │
│  sensor UDP :5002     │──── JSON sensor data ───────────────────────►│
└──────────┬────────────┘                                              │
           │                                              ┌────────────▼─────────┐
           │ WebSocket (WSS)                              │  darc-pilot (browser)│
           │                                              │  ─────────────────── │
┌──────────▼────────────┐                                │  raceWebTransport    │
│  darc-signal          │◄─── WebSocket (WSS) ──────────►│  candidates          │
│  (Node.js, Cloud Run) │                                │                      │
│  ─────────────────    │────── relay binary frames ────►│  chunk reassembly    │
│  register / connect   │                                │  handleVideoChunk    │
│  forward pilot IP     │                                │                      │
│  relay binary in      │                                │  VideoDecoder        │
│  fallback mode        │                                │  → <canvas>          │
└───────────────────────┘                                └──────────────────────┘
```

**Primary video path (P2P):**
```
webcam → FFmpeg H.264 → RTP UDP → PyAV demux → chunk → QUIC DATAGRAM → reassemble → WebCodecs → canvas
```

**Fallback video path (relay):**
```
webcam → FFmpeg H.264 → RTP UDP → PyAV demux → chunk → WebSocket → signal server → WebSocket → reassemble → WebCodecs → canvas
```

Both paths use identical chunk format; the pilot's reassembly and decoder path is the same.

---

## Component Inventory

### Production-worthy components (`packages/`)

---

#### `darc-signal` — Signaling and relay server

**File:** `packages/signal/index.js`

**Responsibility:**
- Register robots (their WebTransport candidates + TLS fingerprint)
- Broker pilot connections (forward candidates + fingerprint to pilot)
- Relay the pilot's real IP to the agent (for NAT hole-punch probes)
- Forward binary video chunks in relay fallback mode
- Maintain fleet presence for the fleet web UI

**WebSocket message protocol:**

| Direction | Message | Fields |
|---|---|---|
| Robot → server | `register` | `robotId`, `certFingerprint`, `candidates: [{url, label, priority, needsProbe}]`, `p2pHint` |
| Pilot → server | `connect` | `robotId` |
| Pilot → server | `relay-request` | `robotId` (P2P failed, activate relay) |
| Pilot → server | `probe-request` | `robotId` (retrying P2P from relay — punch again) |
| Server → pilot | `ready` | `candidates`, `certFingerprint`, `p2pHint` |
| Server → pilot | `unreachable` | `reason` |
| Server → pilot | `peer-disconnected` | — |
| Server → robot | `pilot-connected` | `pilotIp` (real IP from `X-Forwarded-For`) |
| Server → robot | `punch` | `pilotIp` — reopen the hole *without* tearing down video sinks |
| Server → robot | `peer-disconnected` | — |
| Server → robot | `relay-mode` | — (switch video to WebSocket) |
| Server → robot (binary) | video chunks | forwarded verbatim from pilot's WebSocket |
| Server → pilot (binary) | video chunks | forwarded verbatim from robot's WebSocket |

`punch` exists separately from `pilot-connected` because the latter also tells
the agent to drop its video sinks. Reusing it for a P2P retry would kill the
relay that is currently carrying video.

`p2pHint` is `'likely' | 'lan-only' | 'none'` — the agent's own read on whether
P2P can work at all, derived from NAT classification and whether a port mapping
succeeded. The pilot turns it into a deadline (10s / 4s / 2s), so a
provably-hopeless attempt fails fast while a plausible one gets the full window.

**Cloud Run specifics:**
- `X-Forwarded-For` header used for real client IP (load balancer sets `remoteAddress` to `169.254.169.126`)
- 30s WebSocket pings prevent Cloud Run's 60-minute idle timeout
- `--max-instances 1` (in-memory session state)

**Deployed at:** `wss://darc-signal-qjsonun6gq-ew.a.run.app`

---

#### `darc-agent` — Robot-side relay daemon

**Files:** `packages/agent/`

| File | Responsibility |
|---|---|
| `agent.py` | Entry point: socket binding, candidate gathering, startup sequence, callback wiring |
| `cert.py` | Generate ECDSA P-256 self-signed TLS cert with SAN (IPv4 + IPv6) |
| `stun.py` | STUN client with retransmission, NAT classification, local address discovery |
| `portmap.py` | Router port mapping via PCP, NAT-PMP, and UPnP-IGD |
| `transport.py` | aioquic WebTransport server (`WebTransportServer`, `DARCProtocol`, `_Session`) |
| `peer.py` | `Relay` class: video chunking + sender, sensor relay, command handling |
| `signaling.py` | `SignalingClient`: WebSocket to darc-signal, relay mode send |

**Startup sequence (critical ordering):**
1. `bind_sockets()` binds the UDP sockets QUIC will use — IPv4 on `0.0.0.0:4433`, and IPv6 on `[::]:4433` when available (`IPV6_V6ONLY`, so the two don't collide on the same port)
2. `gather_candidates()` discovers every reachable address (below)
3. Generate TLS cert with all discovered IPs in `SubjectAlternativeName` → required by Chrome's `serverCertificateHashes` verifier
4. Start `Relay` + WebTransport server **on the already-bound sockets**
5. Connect to signal server, register with candidates + fingerprint + `p2pHint`

Sockets are bound first, before anything else, because STUN has to run on the
very socket that later receives QUIC. An earlier version bound a throwaway
socket for STUN, closed it, and let aioquic rebind the port — which made the
advertised reflexive address correct only if the NAT happened to reissue the
same external port. It usually did on port-preserving home routers and silently
did not on strict ones. Same socket, never rebound, no guess.

**WebTransport server:**
- Runs on caller-supplied pre-bound sockets (one `QuicServer` per address family) rather than binding its own via aioquic's `serve()`
- Accepts HTTP/3 CONNECT at path `/darc`
- `max_datagram_frame_size = 65536` for QUIC DATAGRAM support
- Self-signed ECDSA P-256 cert, 13-day validity (Chrome's `serverCertificateHashes` limit is 14 days)

**TLS certificate requirements** (Chrome's `serverCertificateHashes` verifier):
- ECDSA P-256 key
- Validity ≤ 14 days (we use 13)
- `SubjectAlternativeName` extension must be present with the server's IP(s)
- Fingerprint = SHA-256 of DER-encoded cert bytes

**NAT traversal — why this is not ICE**

The pilot is a browser using the `WebTransport` API, which is strictly
client→server HTTP/3. The browser has no ICE agent: it cannot gather
candidates, cannot send STUN connectivity checks, and cannot control or observe
its own source port. Only `RTCPeerConnection` has an ICE agent in the browser.

So DARC's traversal problem is not symmetric peer connectivity — it is the
narrower **"make the agent reachable as a server."** Everything below follows
from that, and it bounds what is achievable:

| Agent-side NAT | Reachable | How |
|---|---|---|
| Public IP / manually forwarded | ✓ | host / srflx candidate |
| UPnP / NAT-PMP / PCP router | ✓ | explicit port mapping |
| Both ends IPv6 | ✓ | no NAT at all |
| Full-cone | ✓ | srflx candidate |
| Address-restricted | ✓ | srflx + hole punch |
| Port-restricted | ✗ | would need the browser's ephemeral source port, which is unknowable |
| Symmetric / CGNAT | ✗ | external mapping differs per destination |

The last two rows are architectural, not missing work. They relay.

**Candidate gathering** (`gather_candidates`, in priority order):

| Label | Priority | needsProbe | Source |
|---|---|---|---|
| `host` | 240 | no | Private/public IPv4 on any interface — wins instantly on a shared LAN |
| `host6` | 200 | yes | Globally routable IPv6; no NAT, but may want a firewall pinhole |
| `portmap` | 220 | no | PCP / NAT-PMP / UPnP mapping, only if the router's external address is globally routable |
| `srflx` | 150 | yes | STUN reflexive address, only when the NAT is not symmetric |

`needsProbe` tells the pilot which candidates depend on a hole being punched
first. Those are held back ~400ms; the rest fire immediately, so the common
same-LAN case doesn't wait on a hole punch it never needed.

**Router port mapping** (`portmap.py`) — tried before falling back to STUN
inference. PCP (RFC 6887) and NAT-PMP (RFC 6886) are UDP to `gateway:5351`;
UPnP-IGD is SSDP discovery plus a SOAP `AddPortMapping`. All three are
implemented directly rather than via miniupnpc, to keep the agent on pure-Python
wheels for the eventual ARM cross-compile. An explicit mapping beats hole
punching: it also covers port-restricted NAT, and it doesn't expire the way an
inferred mapping does.

A router will happily install a mapping and report an external address that is
itself behind another NAT — RFC 1918 on a double-NAT LAN, or `100.64.0.0/10`
under CGNAT. That mapping is still useful (it removes the inner NAT from the
path, which can make the reflexive candidate work) but the address must not be
advertised. The predicate for this is `ipaddress.is_global`, **not** `is_private`
— the latter does not flag CGNAT space.

**STUN** (`stun.py`) — two servers on different operators (Google, Cloudflare),
each with a compressed RFC 5389 retransmission ladder. Two operators rather than
one because comparing the two answers is what classifies the NAT:

- both report the same `ip:port` → cone; the reflexive candidate is usable
- same IP, different ports → **symmetric**; the mapping is chosen per
  destination, so the port STUN saw is not the port the pilot would arrive on.
  The candidate is dropped rather than advertised, since it can only waste a
  slot in the pilot's race.

**NAT hole-punching** (`start_probing`):
- On `pilot-connected`, the agent sends a small UDP packet from the WebTransport
  socket toward the pilot's IP, repeated every 250ms for 12s
- Repeated rather than fired once: a lone UDP packet can drop, and Chrome
  retries its QUIC handshake with backoff, so the Initial can arrive seconds later
- One destination port, not a list of guesses. For address-restricted NAT — the
  one type this helps — filtering is by source *address* and the port is
  irrelevant. For port-restricted NAT, where the port would matter, it is
  unknowable. Probing three arbitrary ports out of ~16k ephemeral ones was
  guesswork that bought nothing.
- Probing stops as soon as a session is established

**Latency guarantee — frame sender design:**
- PyAV thread writes each decoded frame to a single `_latest_frame` slot (overwrites previous)
- `_frame_sender` async coroutine wakes on `asyncio.Event`, reads the slot
- Before sending: calls `flush_datagrams()` which clears aioquic's `_datagrams_pending` list — discards any chunks from the previous frame that haven't been transmitted yet
- Mid-frame: if `_latest_frame` becomes non-None during the chunk loop, abandons current frame
- Result: pilot always sees the most recent frame regardless of network speed

**Relay fallback:**
- `signaling.send_binary(data)` sends binary over the agent's WebSocket to the signal server
- In relay mode: `relay.send_binary = signaling.send_binary`, `relay.flush_send_queue = None`
- Same chunk format — pilot handles identically

**Configuration flags:**
```
--robot-id           <string>    e.g. mac-robot-01
--signal-url         <wss://...> signal server
--webtransport-port  <int>       UDP port for WebTransport (default: 4433)
--webtransport-host  <host>      Override discovery (skip STUN, use this IP)
--video-port         <int>       RTP video input (default: 5000)
--sensor-port        <int>       Sensor UDP input (default: 5002)
--no-port-mapping                Skip PCP/NAT-PMP/UPnP
--no-ipv6                        Do not listen on or advertise IPv6
```

**Dependencies:** `av` (PyAV), `websockets`, `aioquic`, `cryptography`, `ifaddr`

---

#### `darc-pilot` — Browser operator application

**Files:** `packages/pilot/index.html`, `packages/pilot/pilot.js`

**P2P connection flow:**
1. Connects to signal server, sends `connect`
2. Receives `ready` with candidates + fingerprint + `p2pHint`
3. Calls `initDecoder()` to reset the WebCodecs VideoDecoder
4. Calls `connectWebTransport(candidates, fingerprint, hint)`:
   - Sorts candidates by `priority`, descending
   - Fires `needsProbe: false` candidates immediately; holds the rest 400ms so the agent's probes can land first
   - Races all `wt.ready` promises; first to resolve wins, every other attempt is closed
   - Deadline from `p2pHint` (10s / 4s / 2s); on timeout or total failure → sends `relay-request`
5. In relay mode: receives binary `ArrayBuffer` from WebSocket → `handleVideoChunk`
6. In P2P mode: reads `wt.datagrams.readable` → `handleVideoChunk`

**No attempt outlives the deadline.** The race owns every `WebTransport` it
creates and closes all of them on timeout. This is load-bearing: the agent
rewires its video output to QUIC datagrams the instant a session is accepted,
so a straggler connecting after the pilot has moved to relay would hand the
agent a P2P session the pilot has no datagram reader for — video would stop
dead with nothing to recover it.

**Relay is not a one-way door.** While relaying, the pilot retries P2P every
30s: it sends `probe-request` to reopen the agent's NAT hole, then re-runs the
same race. A failed retry costs nothing and cannot disturb the working relay,
because the agent only switches its output when a session is actually accepted.
On success the pilot calls `attachSession()` — the same wiring the initial
connect uses — and reports "Upgraded to direct connection". NAT state,
interfaces, and networks all change under a robot in the field.

**Chunk reassembly (`handleVideoChunk`):**
- `_frameChunks` Map: `frame_id → {chunks[], received, total, isKeyframe}`
- Stores each chunk by `chunk_idx`
- When `received === total`: concatenates all chunks, calls `handleVideoFrame(buf, isKeyframe)`
- Evicts incomplete frames with `frame_id` more than 30 behind the current one (stale/lost frames)

**Video decode:**
```javascript
decoder.configure({
    codec: 'avc1.42001f',          // H.264 Baseline Level 3.1
    optimizeForLatency: true,      // no reorder wait — render immediately
    hardwareAcceleration: 'prefer-hardware',
});
```
`optimizeForLatency: true` is critical — without it the browser may buffer frames before rendering.

**Commands (P2P mode only):**
- Pilot creates a bidirectional stream via `wt.createBidirectionalStream()`
- Commands sent as newline-delimited JSON: `{"type": "snapshot", "ts": <ms>}`
- Agent writes acks to the same stream: `{"type": "ack", "cmd": "snapshot", "ts": ...}`

**Note:** Commands are not forwarded in relay mode (no bidi stream without WebTransport). This is a known prototype limitation — in relay mode, Space bar snapshot has no effect. Sensor data still works (flows via JSON WebSocket in both modes).

---

### Prototype-only components (`sim/`)

These simulate the robot camera and sensor system. In production, a real robot's camera pipeline and sensor topics replace them.

#### `sim/video-source.sh`
Captures the Mac webcam and streams H.264 Baseline RTP to `127.0.0.1:5000`.

```bash
ffmpeg \
  -fflags nobuffer \
  -f avfoundation -framerate 30 -video_size 1280x720 -i "${DEVICE}" \
  -pix_fmt yuv420p \
  -vcodec libx264 -tune zerolatency -preset ultrafast -profile:v baseline \
  -g 15 -an -flush_packets 1 \
  -f rtp "rtp://127.0.0.1:${PORT}"
```

Key flags: `-g 15` (keyframe every 15 frames = 0.5s at 30fps), `-tune zerolatency`, `-profile:v baseline` (required for `avc1.42001f`), `-pix_fmt yuv420p` (Baseline requires 4:2:0).

#### `sim/sensor-source.py`
Increments a counter and sends each value as UTF-8 UDP to `127.0.0.1:5002` at 10 Hz.

---

## Data Formats

### Video chunks (primary format — same for P2P and relay paths)

Each H.264 access unit from PyAV is split into 1000-byte chunks to fit within QUIC DATAGRAM's MTU limit (~1200 bytes after QUIC/H3 framing overhead). Each chunk is sent as one QUIC DATAGRAM (P2P) or one binary WebSocket frame (relay).

```
Byte 0:     flags — 0x80 = keyframe, 0x00 = delta frame
Bytes 1-2:  frame_id (uint16 big-endian, rolls at 65535)
Bytes 3-4:  chunk_idx (uint16 big-endian, 0-based)
Bytes 5-6:  total_chunks (uint16 big-endian)
Bytes 7+:   H.264 Annex B payload slice (≤ 1000 bytes)
```

**Why 1000-byte chunks?** QUIC DATAGRAM frames must fit in a single UDP packet. The path MTU is typically 1200–1500 bytes; after QUIC, HTTP/3, and WebTransport framing, roughly 1200 bytes remain for the DATAGRAM payload. A 1000-byte cap is conservative and guarantees delivery on any reasonable path. H.264 keyframes at 720p can be 20–100 KB — without chunking they would be silently dropped by aioquic.

### Sensor data (robot → pilot, JSON)
```json
{ "type": "sensor", "data": "42" }
```

### Commands (pilot → robot, JSON via bidi stream or WebSocket)
```json
{ "type": "snapshot", "ts": 1723456789123 }
```

### Acknowledgments (robot → pilot, JSON)
```json
{ "type": "ack", "cmd": "snapshot", "ts": 1723456789123 }
```

---

## Repository Structure

```
darc/
├── packages/
│   ├── signal/            # darc-signal (Node.js, Cloud Run)
│   │   ├── index.js       # WebSocket server, relay logic
│   │   ├── fleet.html     # Fleet web UI (lists online robots)
│   │   ├── pilot/         # pilot static files (copied in at build time)
│   │   ├── Dockerfile
│   │   ├── deploy.sh      # GCLOUD_PROJECT=darc-platform ./deploy.sh
│   │   └── DEPLOY.md
│   ├── agent/             # darc-agent (Python)
│   │   ├── agent.py       # Entry point, socket binding, candidate gathering
│   │   ├── cert.py        # ECDSA P-256 TLS cert generation (IPv4 + IPv6 SAN)
│   │   ├── stun.py        # STUN client, NAT classification, local IP discovery
│   │   ├── portmap.py     # PCP / NAT-PMP / UPnP-IGD router port mapping
│   │   ├── transport.py   # aioquic WebTransport server on pre-bound sockets
│   │   ├── peer.py        # Relay class: video chunking, sensor, commands
│   │   ├── signaling.py   # SignalingClient
│   │   └── requirements.txt  # av, websockets, aioquic, cryptography, ifaddr
│   └── pilot/             # darc-pilot (HTML/JS, served by darc-signal)
│       ├── index.html
│       └── pilot.js
├── sim/                   # Robot simulation — NOT part of DARC
│   ├── video-source.sh    # FFmpeg webcam → RTP H.264 UDP :5000
│   └── sensor-source.py   # Counter → UDP :5002 at 10 Hz
├── robot.sh               # Starts sensor-sim + video-sim + agent (robot Mac)
├── CLAUDE.md
├── SPEC.md
└── PROTOTYPE.md
```

### Starting the robot

```bash
./robot.sh [robot-id]
# Defaults: robot-id = mac-robot-01, signal = wss://darc-signal-qjsonun6gq-ew.a.run.app
```

The script kills any stale processes, then starts sensor-sim, video-sim, and agent.

### Deploying the signal server

```bash
cd packages/signal
GCLOUD_PROJECT=darc-platform ./deploy.sh
```

The pilot static files are copied from `packages/pilot/` into the signal build context automatically.

---

## Known Limitations and Open Issues

### Relay path — unbounded latency on congested links
On the WebSocket relay path, the TCP layer can buffer chunks if the signal server or the final WebSocket link is congested. The `flush_datagrams()` mechanism that prevents queue buildup is only effective on the P2P QUIC path. On relay, the signal server could add growing latency under sustained load. **For the prototype this is acceptable** — the relay is a last resort for unreachable robots. In production, a proper TURN relay or direct QUIC relay would replace it.

### Commands not forwarded in relay mode
The pilot's bidirectional JSON command stream is a WebTransport stream; it doesn't exist in relay mode. Snapshot (Space bar) silently does nothing. Sensor data still flows. Fixing this requires routing commands via the signaling WebSocket JSON path (signal server relays JSON pilot→robot and robot→pilot).

### Symmetric NAT and port-restricted NAT cannot do P2P
Architectural, not a gap — see the traversal table above. Both need something
the browser cannot provide (its own ephemeral source port) or something the NAT
refuses to provide (a stable external mapping). Symmetric NAT is now *detected*
at startup, so the pilot fails over in ~2s instead of waiting the full 10.
Port-restricted NAT is not distinguishable from address-restricted without a
cooperating peer, so it still burns the full window before relaying.

The real fix for both is a router port mapping, which `portmap.py` now attempts
first. Where that is unavailable (carrier networks, locked-down corporate LANs)
the relay is genuinely the only option.

### One pilot per robot
Session state is in-memory on the single Cloud Run instance. The signal server tracks one robot WebSocket and one pilot WebSocket per robot ID. Multi-pilot monitoring is not implemented.

---

## Success Criteria (all met)

1. **Single encode chain** — no decode/re-encode in the agent path. FFmpeg encodes once; WebCodecs decodes once.
2. **No jitter buffer** — frames rendered as they arrive; `optimizeForLatency: true` confirmed.
3. **WebTransport P2P works on LAN and broadband** — confirmed.
4. **No latency growth on mobile** — frame sender drops stale frames, flushes aioquic queue before each send; latency stays flat.
5. **Relay fallback works** — automatic after 10s timeout; no user intervention.
6. **Sensor data and commands flow** — counter visible on pilot page; snapshot round-trip works.
7. **Agent decoupled from video source** — stopping FFmpeg pauses video but does not crash the agent.

---

## Next Steps

### Immediate (prototype improvements)
1. **Relay-mode commands** — route JSON commands via signaling WebSocket in relay mode (signal server relays pilot→robot JSON in both directions, not just binary)
2. **Latency measurement** — capture wall-clock timestamp in the frame chunk header; display end-to-end latency in the pilot UI
3. **Relay status in UI** — currently shows "Waiting for video… (relay)" before video starts, then just "Connected" — should persistently indicate relay mode, and surface which candidate label won
4. **Port mapping renewal** — mappings are requested with a 3600s lease but never renewed, so a session running longer than an hour can lose its `portmap` candidate. Renew at half the granted lifetime, and release on shutdown.
5. **Re-run discovery on network change** — candidates are gathered once at startup. A robot that changes interface (WiFi → cellular) keeps advertising stale ones until restarted.
6. **Port forwarding instructions** — when relay activates and no port mapping was available, tell the user which port to forward manually

### Production path
1. **Port agent to C with MsQuic** — Python/aioquic is prototype-quality; MsQuic handles QUIC on ARM Linux embedded targets
2. **True TURN relay** — replace WebSocket relay fallback with standard TURN (coturn or Cloudflare TURN); use standard ICE candidate exchange
3. **Auth and session security** — robot identity keys, operator session tokens, RBAC
4. **Fleet registry** — persistent robot identities, owner accounts, presence service
5. **Multi-camera** — one QUIC connection, multiple DATAGRAM streams (keyed by stream/camera ID)
6. **ROS2 agent form factor** — native ROS2 node wrapper around the agent core
