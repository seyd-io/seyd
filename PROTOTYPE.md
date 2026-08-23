# DARC Prototype — Mac-to-Mac Teleoperation

## Current Status (as of 2026-08-22)

**Phase 2 is implemented and working.** The prototype now runs the production transport architecture — WebTransport over QUIC (P2P) as the primary path, with automatic fallback to WebSocket relay through the signal server when P2P cannot be established. Both paths are tested on real hardware.

| Scenario | Works | Notes |
|---|---|---|
| Robot + pilot on same LAN | ✓ | Host candidate wins immediately, sub-ms NAT traversal |
| Robot on broadband, pilot on broadband | ✓ | STUN candidate, hole punching |
| Robot on iPhone hotspot (CGNAT), pilot on broadband | ✓ (relay) | Symmetric NAT blocks P2P; auto-falls back to WebSocket relay after 10s |
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
| Robot → server | `register` | `robotId`, `certFingerprint`, `candidates: [{url, label}]` |
| Pilot → server | `connect` | `robotId` |
| Pilot → server | `relay-request` | `robotId` (P2P failed, activate relay) |
| Server → pilot | `ready` | `candidates`, `certFingerprint` |
| Server → pilot | `unreachable` | `reason` |
| Server → pilot | `peer-disconnected` | — |
| Server → robot | `pilot-connected` | `pilotIp` (real IP from `X-Forwarded-For`) |
| Server → robot | `peer-disconnected` | — |
| Server → robot | `relay-mode` | — (switch video to WebSocket) |
| Server → robot (binary) | video chunks | forwarded verbatim from pilot's WebSocket |
| Server → pilot (binary) | video chunks | forwarded verbatim from robot's WebSocket |

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
| `agent.py` | Entry point: startup sequence, wiring all callbacks |
| `cert.py` | Generate ECDSA P-256 self-signed TLS cert with SAN |
| `stun.py` | Async STUN client + local LAN IP discovery |
| `transport.py` | aioquic WebTransport server (`WebTransportServer`, `DARCProtocol`, `_Session`) |
| `peer.py` | `Relay` class: video chunking + sender, sensor relay, command handling |
| `signaling.py` | `SignalingClient`: WebSocket to darc-signal, relay mode send |

**Startup sequence (critical ordering):**
1. Discover local LAN IPs → `get_local_ips()` — host candidates
2. STUN from a temp UDP socket on port 4433 (before aioquic binds) → `get_stun_address(wt_port)` — STUN-reflexive candidate. STUN is done BEFORE aioquic binds because: (a) we can't use `loop.add_reader` on aioquic's socket without breaking its internal read handler, and (b) many NATs preserve the external port mapping when the same local port quickly rebinds.
3. Generate TLS cert with all discovered IPs in `SubjectAlternativeName` → required by Chrome's `serverCertificateHashes` verifier
4. Start `Relay` + WebTransport server on UDP :4433
5. Connect to signal server, register with candidates + fingerprint

**WebTransport server:**
- Listens on UDP :4433 (aioquic)
- Accepts HTTP/3 CONNECT at path `/darc`
- `max_datagram_frame_size = 65536` for QUIC DATAGRAM support
- Self-signed ECDSA P-256 cert, 13-day validity (Chrome's `serverCertificateHashes` limit is 14 days)

**TLS certificate requirements** (Chrome's `serverCertificateHashes` verifier):
- ECDSA P-256 key
- Validity ≤ 14 days (we use 13)
- `SubjectAlternativeName` extension must be present with the server's IP(s)
- Fingerprint = SHA-256 of DER-encoded cert bytes

**NAT hole-punching:**
- On `pilot-connected`, agent sends small UDP probes from the WebTransport socket (port 4433) to the pilot's IP on common ports (443, 4433, 8080)
- This creates a NAT mapping allowing inbound QUIC Initial packets from the pilot
- Works for full-cone and address-restricted NAT
- Does NOT work for symmetric NAT (carrier CGNAT — use relay fallback instead)

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
--webtransport-host  <host>      Override STUN (skip STUN, use this IP)
--video-port         <int>       RTP video input (default: 5000)
--sensor-port        <int>       Sensor UDP input (default: 5002)
```

**Dependencies:** `av` (PyAV), `websockets`, `aioquic`, `cryptography`, `ifaddr` (optional; falls back to socket trick for IP discovery)

---

#### `darc-pilot` — Browser operator application

**Files:** `packages/pilot/index.html`, `packages/pilot/pilot.js`

**P2P connection flow:**
1. Connects to signal server, sends `connect`
2. Receives `ready` with candidates + fingerprint
3. Calls `initDecoder()` to reset the WebCodecs VideoDecoder
4. Calls `connectWebTransport(candidates, fingerprint)`:
   - Waits 400ms (gives agent NAT probes time to reach pilot)
   - Creates one `WebTransport` per candidate simultaneously
   - Races all `wt.ready` promises; first to resolve wins, others closed
   - 10-second overall timeout; on timeout/failure → sends `relay-request`
5. In relay mode: receives binary `ArrayBuffer` from WebSocket → `handleVideoChunk`
6. In P2P mode: reads `wt.datagrams.readable` → `handleVideoChunk`

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
│   │   ├── agent.py       # Entry point, startup sequence, callback wiring
│   │   ├── cert.py        # ECDSA P-256 TLS cert generation
│   │   ├── stun.py        # Async STUN client + local IP discovery
│   │   ├── transport.py   # aioquic WebTransport server
│   │   ├── peer.py        # Relay class: video chunking, sensor, commands
│   │   ├── signaling.py   # SignalingClient
│   │   └── requirements.txt  # av, websockets, aioquic, cryptography
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

### STUN and symmetric NAT
The STUN-reflexive candidate fails for robots behind symmetric carrier CGNAT (common on 5G/4G). The relay fallback handles this case, but the 10-second P2P timeout is noticeable before video starts. A proper TURN relay would provide video immediately with relay quality.

### STUN pre-binding window
STUN runs on a temporary socket before aioquic binds. The NAT mapping created by STUN may expire (NATs typically give UDP 30–120s) and may differ from the mapping aioquic creates when it binds the same port. On most residential NATs with port-preservation, they match. On strict or load-balanced NATs, they may not. See `stun.py` for the SO_REUSEADDR/REUSEPORT workaround.

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
3. **Relay status in UI** — currently shows "Waiting for video… (relay)" before video starts, then just "Connected" — should persistently indicate relay mode
4. **Port forwarding instructions** — if P2P fails and relay activates, show a hint to the user about UPnP or manual UDP :4433 forwarding

### Production path
1. **Port agent to C with MsQuic** — Python/aioquic is prototype-quality; MsQuic handles QUIC on ARM Linux embedded targets
2. **True TURN relay** — replace WebSocket relay fallback with standard TURN (coturn or Cloudflare TURN); use standard ICE candidate exchange
3. **Auth and session security** — robot identity keys, operator session tokens, RBAC
4. **Fleet registry** — persistent robot identities, owner accounts, presence service
5. **Multi-camera** — one QUIC connection, multiple DATAGRAM streams (keyed by stream/camera ID)
6. **ROS2 agent form factor** — native ROS2 node wrapper around the agent core
