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
| Quality under motion | ✓ | Bitrate capped; was unbounded CRF at 8–14 Mbps |
| Independent packet loss | ✓ | 99.3% of frames delivered at 5% chunk loss (71.9% without FEC) |
| Bursty packet loss | ~ | ~90% delivered at 5%/burst-3; keyframes never lost, so corruption self-heals within one GOP |

### Measured video resilience

`latency` profile (960×540, 1.5 Mbps + parity = 1.9 Mbps total), 30 fps, synthetic
`testsrc2` motion fixture over the relay path, 10 s per run. `clean` is the share
that arrived without needing FEC — i.e. what delivery *would* have been without it.

| injected loss | clean | recovered by FEC | delivered | keyframes lost |
|---|---|---|---|---|
| 1% independent | 90.7% | 9.3% | **100.0%** | 0 |
| 5% independent | 71.9% | 27.4% | **99.3%** | 0 |
| 10% independent | 55.5% | 41.5% | **97.0%** | 0 |
| 5%, bursts of 2 | 80.3% | 15.1% | 95.3% | 0 |
| 5%, bursts of 3 | 85.7% | 4.7% | 90.3% | 0 |
| 5%, bursts of 6+ | 87.7% | 3.3% | ~91% | 0 |

For reference, the pre-fix baseline was 19% of frames arriving whole at 5% loss,
with every loss smearing for up to 0.5 s.

**Zero keyframes were lost in any run**, including 10% loss and bursts of 10.
Keyframes are ~18 data chunks with 50% parity (k=9), which survives bursts
comfortably — so however bad the delta-frame loss, the picture fully resets every
GOP (1 s) and corruption cannot accumulate.

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
| Pilot → server | `qos` | `robotId`, `profile` |
| Server → robot | `qos` | `profile` — forwarded verbatim |
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

`qos` goes through the signal server rather than the WebTransport stream because
that is the only path that reaches the agent in relay mode, where there is no
pilot→robot JSON channel at all. The signal server has no opinion about QoS; it
stores the name on the session (so it can be delivered before the first frame is
encoded) and forwards it.

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
| `fec.py` | Reed-Solomon over GF(256) + the video chunk wire format |
| `qos.py` | QoS profile table (DARC's half — transport targets, no resolution) |
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
- **Admission control happens before the first chunk is sent.** If the backlog
  (`pending_bytes()`) exceeds the profile's `drop_threshold_bytes`, a delta frame
  is dropped whole and nothing is transmitted.
- **Keyframes are never dropped.** If a keyframe faces a backlog, `drop_pending()`
  clears the queue and the keyframe goes. This is the only case where discarding
  is free — an IDR makes every queued chunk from the previous GOP irrelevant.
- Once a frame is committed, every one of its chunks is sent, in one batch with a
  single `transmit()` call.
- Result: the pilot always sees the most recent *complete* frame.

An earlier version instead flushed the datagram queue before every frame and
abandoned frames mid-chunk-loop. Both emitted partial frames, which is the worst
available outcome: the bandwidth was already spent, the pilot must discard the
frame anyway, and because H.264 delta frames reference their predecessors one
torn frame corrupts every later frame until the next keyframe. **Skipping a frame
cleanly costs one frame; tearing one costs a GOP.** The threshold is a byte budget
rather than "queue non-empty" because aioquic's pending list also grows simply
from the pacer spacing packets out.

Note this bounds *latency*, not bandwidth — the encoder already spent a dropped
frame's bits. Only the publisher's bitrate cap bounds bandwidth; both are needed.

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
--qos-profile        <name>      latency | balanced | quality (default: balanced)
--publisher-control-port <int>   UDP port the video publisher listens on (5003)
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
- `_frames` Map: `frame_id → {data[], parity[], dataRx, parityRx, …}`
- **Recovery is eager**: the moment `dataRx + parityRx >= n` the frame is
  solvable, so FEC adds no latency of its own. `dataRx === n` skips it entirely.
- Every frame-id comparison goes through `DARCFec.int16Delta()`, a wrap-aware
  *signed* 16-bit difference. The previous unsigned version had a real bug: one
  reordered chunk from an older frame created a map entry, and the eviction sweep
  then computed `(oldId − currentId) & 0xFFFF ≈ 65500 > 30` for the frame being
  actively assembled and deleted it.
- Chunks older than the newest seen by more than `MAX_REORDER = 4` are rejected
  rather than admitted.
- **Frames are gated into decode order** via `_lastDecodedId`. Without this a
  late-recovered frame would be fed to the decoder after its successor and
  corrupt decoder state. Under `optimizeForLatency: true` with no B-frames,
  decode order equals display order, so a strict monotonic gate is correct.
- A frame is closed out (counted lost) on whichever comes first: its per-frame
  close-out timer, or a newer frame decoding. This replaced a 30-frames-behind
  eviction rule that delayed loss accounting by a full second, which is why loss
  used to be invisible.

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
Reads `DARC_QOS_PROFILE` (default `balanced`) and maps it to encoder settings —
see the QoS table above.

```bash
ffmpeg -fflags nobuffer \
  -f avfoundation -framerate ${FPS} -video_size ${W}x${H} -i "${DEVICE}" \
  -pix_fmt yuv420p \
  -c:v libx264 -tune zerolatency -preset ${PRESET} -profile:v baseline \
  -b:v ${KBPS}k -maxrate ${KBPS}k -bufsize ${BUFK}k \
  -g ${GOP} -keyint_min ${GOP} -bf 0 -x264-params "scenecut=0" \
  -an -flush_packets 1 -max_delay 0 \
  -f rtp "rtp://127.0.0.1:${PORT}"
```

**The bitrate cap is the single most important flag here.** Without `-b:v`/
`-maxrate`/`-bufsize`, libx264 runs in default CRF≈23 constant-*quality* mode:
measured 7.9–13.7 Mbps at 720p30, unbounded and spiking on motion. No cellular
uplink sustains that, so motion produced genuine packet loss — the root cause of
the quality collapse this profile system exists to fix.

`bufsize` = 100 ms × maxrate, and it is a *latency* knob as much as a quality one.
ffmpeg's default (`bufsize == maxrate`, one second) lets a motion burst emit a
whole second of extra bits, which the modem queue absorbs as ~1 s of added
glass-to-glass latency or drops as loss. One frame (33 ms) is too tight — an IDR
legitimately needs 3–5× an average frame. ~100 ms fits one IDR without allowing a
deep queue.

Other choices: `-g 30` rather than 15 (an IDR costs ~5× a P-frame, so halving the
rate returns 15–20% of the budget to P-frames, at a 1 s ceiling on error
propagation and join latency); `-preset veryfast` for the higher profiles because
`ultrafast` sets `aq-mode=0` and adaptive quantisation is exactly what keeps dark
and low-contrast regions readable when bits are scarce; `scenecut=0` so IDR
placement is strictly periodic, since an unpredictable spike is what we are
eliminating. Deliberately *not* `nal-hrd=cbr`, which pads to hit the rate exactly
and spends scarce uplink on filler.

`VIDEO_DEVICE=lavfi` swaps in a synthetic `testsrc2` source: continuous motion, so
a permanent worst case, and byte-reproducible, so loss and bitrate measurements
are comparable between runs. It needs `-re` — without it ffmpeg generates frames
as fast as the CPU allows (measured ~2450 fps / 157 Mbps), which floods the relay
and makes every measurement meaningless.

#### `sim/sensor-source.py`
Increments a counter and sends each value as UTF-8 UDP to `127.0.0.1:5002` at 10 Hz.

---

## Data Formats

### Video chunks (primary format — same for P2P and relay paths)

Each H.264 access unit from PyAV is split into 1000-byte chunks, with Reed-Solomon
parity chunks appended. Each chunk is sent as one QUIC DATAGRAM (P2P) or one
binary WebSocket frame (relay).

```
byte  0     bit 7    keyframe
            bits 4-6 fec_type (0 = none, 2 = reed-solomon)
            bits 0-3 format version (currently 1)
bytes 1-2   frame_id      uint16 BE, rolls at 65535
bytes 3-4   chunk_idx     uint16 BE — 0..n-1 data, n..n+k-1 parity
bytes 5-6   total_chunks  uint16 BE = n (DATA chunks only)
byte  7     fec_count     uint8     = k
bytes 8-9   last_len      uint16 BE = real length of data chunk n-1
bytes 10+   payload
```

**Why 1000-byte chunks?** QUIC DATAGRAM frames must fit in a single UDP packet.
Path MTU is typically 1200–1500 bytes; after QUIC, HTTP/3, and WebTransport
framing, roughly 1200 bytes remain. 1000 + the 10-byte header is conservative on
any path. Keyframes are 18–60 KB — without chunking aioquic drops them silently.

**Why `k` is in the header:** the receiver cannot derive the code parameters, and
therefore cannot reconstruct anything, without it.

**Why `last_len` is in the header:** parity is computed over chunks zero-padded to
1000 bytes. If the short final chunk is the one reconstructed it comes back
padded, and there is no other way to recover its true length. Omitting this
appends up to 999 zero bytes to recovered frames — which decoders sometimes
tolerate and sometimes do not, i.e. the worst kind of bug.

**The version nibble makes this a hard cutover.** Agent and pilot must be
deployed together. An unrecognised version is counted (`chunksBadHeader`) and
dropped rather than misparsed into garbage video.

### Forward error correction

`packages/agent/fec.py` (encode) and `packages/pilot/fec.js` (decode) implement
Reed-Solomon over GF(256), field polynomial `0x11d`, with a **Cauchy** generator
matrix `A[i][j] = 1/(i XOR (k+j))`. Cauchy rather than Vandermonde because every
square submatrix is guaranteed invertible — which is exactly the guarantee "any
k losses recover" requires. Both sides derive the matrix from `(n, k)`, so the
construction is part of the wire contract; changing it breaks interop silently,
which is what `tools/fec-check.js` exists to catch.

**Why this is fast enough in pure Python.** The usual objection is that GF(256)
in Python costs 100–200 ms per keyframe and needs numpy — which would violate the
pure-Python-wheels constraint for the ARM port. That objection assumes a per-byte
inner loop. Doing the scalar multiply with `bytes.translate()` and accumulating
with big-integer XOR (both C-speed) measures **0.25 ms** to encode a 20 KB
keyframe at 50% parity and 0.02 ms for a delta frame — 30x faster than the naive
loop. Browser-side decode is 0.02–0.53 ms against a 33 ms frame budget.

Keyframes carry more parity than delta frames: they are 3–5x larger, so their
survival odds at a given chunk-loss rate are much worse, and losing one costs a
whole GOP rather than one frame. Note that `parity_count()` enforces a floor of
k=1 whenever FEC is enabled, so at the small frame sizes the `latency` profile
produces (~6 data chunks) the realised overhead for the `quality` profile is
~14% rather than its nominal 8%.

The agent computes parity over its own transport chunks. It parses no NAL headers
and is indifferent to the payload being H.264, so this stays inside DARC's "pure
byte relay, never transcodes" rule.

### QoS profiles

The link constrains *total bytes on the wire*, not video bitrate, so a profile is
one budget split between pixels, redundancy, and headroom. Before profiles
existed the split was "unbounded pixels, zero redundancy, zero headroom", which
is exactly why motion collapsed the stream.

**The boundary matters.** Per SPEC.md, encoder settings belong to the robot's
video publisher and transport policy belongs to DARC. So a profile is not a
config object both sides read — it is a request DARC makes and the publisher
answers. `packages/agent/qos.py` carries no resolution: DARC says "stay under
1500 kbps with a 100 ms latency budget", and `sim/video-source.sh` maps that onto
resolution, preset and VBV using its own table, because only the publisher knows
its sensor. This is the first concrete piece of SPEC.md's open question on a
codec negotiation API.

| | `latency` | `balanced` | `quality` |
|---|---|---|---|
| **publisher half** | | | |
| resolution / fps | 960×540 / 30 | 1280×720 / 30 | 1280×720 / 30 |
| bitrate cap | 1500 kbps | 3000 kbps | 6000 kbps |
| GOP | 30 (1 s) | 30 (1 s) | 60 (2 s) |
| x264 preset | ultrafast | veryfast | veryfast |
| VBV window | 100 ms | 100 ms | 200 ms |
| **DARC half** | | | |
| FEC delta / keyframe | 25% / 50% | 15% / 30% | 8% / 15% |
| backlog drop threshold | 1 frame | 2 frames | 3 frames |
| pilot close-out (delta/key) | 20/40 ms | 30/60 ms | 50/100 ms |
| on unrecoverable loss | continue | continue | freeze until IDR |
| **total link budget** | ~1.9 Mbps | ~3.5 Mbps | ~6.6 Mbps |

Note the deliberate inversion: the *latency* profile carries the *most*
redundancy. It runs a low video rate and spends the headroom on never losing a
frame; `quality` spends it on pixels and accepts occasional loss.

**FEC costs bandwidth on a bandwidth-limited link.** The overhead and the video
bitrate must be set together as one budget, never tuned independently. If
measured loss *rises* with FEC enabled, lower the video bitrate — not the FEC.

DARC's half applies live, at the next frame boundary (changing FEC mid-frame
would compute parity at a different rate than the header advertises). The
publisher's half currently requires a restart: `sim/video-source.sh` reads
`DARC_QOS_PROFILE` at start. So a live switch reports `publisher: "requested"`
and the pilot shows "(transport only)" rather than implying the camera changed.

### Publisher control interface

The agent sends best-effort JSON over UDP to `127.0.0.1:5003`
(`--publisher-control-port`). DARC defines the message and the port; it does not
implement the publisher. Fire-and-forget and never fatal — a robot whose
publisher ignores this must keep streaming on whatever it was configured with.

```json
{ "type": "video-config", "profile": "latency",
  "maxBitrateKbps": 1500, "latencyBudgetMs": 100, "maxGopMs": 1000 }
```

Also specified, not yet implemented on either side:
`{"type": "request-keyframe"}`. It is useful today (it would cut
join-to-first-frame from up to one GOP down to ~1 RTT) and it is the
prerequisite for intra-refresh — see Known Limitations.

### Telemetry

The agent pushes counters at 1 Hz over the JSON stream:

```json
{ "type": "agent-stats", "frames_in": …, "frames_sent": …,
  "frames_dropped_backlog": …, "frames_skipped_stale": …, "keyframes_forced": …,
  "chunks_sent": …, "parity_sent": …, "bytes_sent": …,
  "pending_bytes": …, "cwnd": …, "srtt_ms": … }
```

The pilot replies with `{"type": "pilot-stats", …}`. Pairing the two is the point:
the pilot cannot see a frame whose chunks were *all* lost, so its own loss figure
is biased low. `chunksRx` against the agent's `chunks_sent` turns an estimate into
a measurement, and the HUD shows both (`true` and `est`).

Press **S** in the pilot to toggle the stats panel. A red border around the canvas
means unrecoverable loss has corrupted the reference chain and what is on screen
cannot be trusted until the next clean keyframe — an operator must never be shown
a smeared or frozen picture without being told.

Neither stats direction works in relay mode (no pilot↔robot JSON path there).

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
│   │   ├── fec.py         # Reed-Solomon GF(256) + chunk wire format
│   │   ├── qos.py         # QoS profiles (DARC half)
│   │   ├── signaling.py   # SignalingClient
│   │   └── requirements.txt  # av, websockets, aioquic, cryptography, ifaddr
│   └── pilot/             # darc-pilot (HTML/JS, served by darc-signal)
│       ├── index.html
│       ├── fec.js         # Reed-Solomon decode + header parse (loads first)
│       └── pilot.js
├── sim/                   # Robot simulation — NOT part of DARC
│   ├── video-source.sh    # FFmpeg webcam → RTP H.264 UDP :5000
│   └── sensor-source.py   # Counter → UDP :5002 at 10 Hz
├── tools/
│   ├── setup-machine.sh   # Dev machine bootstrap
│   ├── fec-vectors.py     # Emit FEC interop vectors (agent side)
│   └── fec-check.js       # Replay them through the pilot decoder
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

### Bursty loss defeats FEC on small delta frames
The `latency` profile's delta frames are only ~6 data chunks, so 25% parity gives
k=2 — and a burst of 3 consecutive losses exceeds it. That is why delivery falls
from 99.3% (independent loss) to ~90% (bursts of 3+) at the same 5% mean rate. The
frames are small relative to the burst length, which is the flip side of the
resolution choice that made them survive individual losses so well.

Not fixed, and the options all cost something: raising delta parity to k=3 needs
~50% overhead; interleaving parity across consecutive frames would cover bursts
but adds a frame of latency, defeating the point. For now this is bounded rather
than solved — keyframes always survive, so the worst case is a smeared second,
never accumulating corruption. Whether it matters depends on whether real cellular
loss is bursty, which the `?burst=` control exists to measure and which has not
yet been characterised on a real 5G link.

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
1. **Characterise real cellular loss** — run the `?burst=` control on the 5G hotspot. Whether loss is bursty or independent decides how much the point above actually costs, and whether delta parity should rise.
2. **Relay-mode commands and telemetry** — route JSON both ways via the signalling WebSocket in relay mode. Currently snapshot silently no-ops and neither stats direction works there.
3. **Keyframe on request** — `{"type":"request-keyframe"}` is specified but implemented on neither side. Worth doing on its own merits (join-to-first-frame drops from a GOP to ~1 RTT) and it is the prerequisite for intra-refresh.
4. **Live publisher reconfiguration** — `sim/video-source.sh` reads `DARC_QOS_PROFILE` once at start, so the encoder half of a profile switch needs a restart. A supervisor listening on the control port would close this; note that a real camera node changes bitrate live through its encoder API, so the restart hiccup is a simulation artefact and must not become a claim about DARC.
5. **Adaptive bitrate** — all the inputs now exist (`cwnd`/`srtt`, true loss, `frames_dropped_backlog`). This is the real answer to a hotspot whose capacity varies 5× minute to minute, and it turns a profile from a fixed setting into a ceiling the link finds its own level under.
6. **Latency measurement** — a send timestamp in the chunk header gives relative jitter and intra-frame spread immediately. True glass-to-glass needs clock sync between the two Macs, which is separate work.
7. **Port mapping renewal** — mappings are requested with a 3600s lease but never renewed, so a session over an hour can lose its `portmap` candidate.
8. **Re-run discovery on network change** — candidates are gathered once at startup, so a robot switching WiFi → cellular advertises stale ones until restarted.

### Production path
1. **Port agent to C with MsQuic** — Python/aioquic is prototype-quality; MsQuic handles QUIC on ARM Linux embedded targets
2. **True TURN relay** — replace WebSocket relay fallback with standard TURN (coturn or Cloudflare TURN); use standard ICE candidate exchange
3. **Auth and session security** — robot identity keys, operator session tokens, RBAC
4. **Fleet registry** — persistent robot identities, owner accounts, presence service
5. **Multi-camera** — one QUIC connection, multiple DATAGRAM streams (keyed by stream/camera ID)
6. **ROS2 agent form factor** — native ROS2 node wrapper around the agent core
