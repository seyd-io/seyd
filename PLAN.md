# Seyd (formerly DARC): Prototype → Product Plan

## Context

The repo is a working proof of concept — Python/aioquic agent, vanilla-JS browser pilot, Node signaling server on Cloud Run — and it has proven the hard claims: single encode chain, no jitter buffer, WebTransport P2P over the public internet, RS-FEC at 99.3% delivery under 5% loss, QoS profiles across the DARC/publisher boundary, and a real Hikvision PTZ demo. The concept is validated; the Python code is not hardened further.

The goal is **remote control as a service**: an agent SDK (embeddable library + LAN daemon + ROS 2), a web pilot SDK, a signaling cloud with login, discovery and fleet view, connection-quality hooks with automatic rate scaling inside a customer-set QoS ceiling, a landing page with demo requests, and the public demo running on the new stack.

**Fixed decisions (owner, 2026-08-28):**
- **Rename DARC → Seyd now.** Domains `seyd.io` / `seydio.com`. Every new-stack identifier is born with the new name: crates `seyd-*`, npm `@seyd/*`, C prefix `seyd_` / `seyd.h`, daemon `seydd`, `/etc/seyd/`, ALPN `seyd/2`, WebTransport path `/seyd`. Legacy Python/relay code keeps "darc" until deleted.
- **Direct first; the cloud relay last** (amended 2026-09-09, ADR 0010). The race runs first, every time; when it fails and the robot allows it, the session is carried by a WebSocket relay on the signal server, shown as relayed in the HUD, the status line and the guidance box, with the direct-path diagnosis kept visible. Metered, separately priced. The QUIC-forwarding relay with its own address (item 22) remains the design for a *fast* relay.
- **Rust core with a C ABI**; every other agent form factor is a thin wrapper. Supersedes SPEC.md's "C + MsQuic".
- **Web pilot SDK first.** iOS/Android/Flutter when a customer needs them. A headless pilot agent is desirable but secondary.
- **Auth provider deferred; no Google lock-in; EU residency likely.** Cloud Run on GCP (`europe-west1`) is the hosting for now, but every cloud component must be portable: plain containers, Postgres + Redis, no GCP-only SDKs or services, auth behind an OIDC abstraction so the identity provider can be chosen later (EU-hosted or self-hosted candidates). Login/sign-up ship regardless — against whichever OIDC provider is plugged in.
- **No timelines/headcount planning** — built with AI tooling; the plan is ordered work.

Guiding principle: **the prototype is the reference implementation, not the product.** `fec.py` + `tools/fec-vectors.py` and `tools/pilot-smoke.py` become the conformance oracle for the new stack; once the demo runs on Rust + `@seyd/web`, the Python agent and the relay code are deleted.

---

## Part 1 — Technology upgrades (what "world-class" means concretely)

Ordered by expected latency/reliability payoff. Each names the current code it replaces and how it is measured.

### 1.1 Sub-frame pipelining at the source
**Today:** `peer.py::_blocking_video_relay` waits for libavformat to hand over a *complete* access unit, then `_frame_sender` chunks, computes parity, and sends. Cost: libavformat's RTP/RTSP reorder + probe latency, plus up to one full frame of serialisation before the first byte leaves.
**Build:** an in-house RFC 6184 depacketizer in `seydd` (RTP/UDP and RTSP-interleaved-TCP via `retina`) that emits NAL units the moment their last packet arrives, and a sender that chunks and transmits **per FEC block** (e.g. 8 data chunks + parity) as bytes arrive instead of per frame. A 720p keyframe (~50 chunks) starts leaving the robot after the first ~8 KB rather than after the last. Frame-boundary invariants stay: a frame is committed at its first block (admission control happens there), and once committed it is sent whole.
**Measure:** agent ingest-to-first-chunk-on-wire per frame; target < 1 ms for delta, < 3 ms for keyframes.

### 1.2 Loss recovery in one RTT: NACK-driven keyframe / LTR, not "wait for the next IDR"
**Today:** a frame FEC cannot rebuild smears the picture until the next periodic IDR (up to 1–2 s); join shows black for up to a GOP (DEMO.md open question 6).
**Build:** the pilot reports each unrecoverable frame id immediately over the control channel (`loss {frame_id, key}`), not in the 1 Hz stats. The agent turns that into `on_recovery_request` to the publisher with a preference order: **LTR/reference-picture-selection** (encoder references the last frame the pilot confirmed decoded — a P-frame-sized fix, no IDR spike) → **intra-refresh** (spread the recovery over N frames) → **IDR**. Also `request-keyframe` on `hello`. The demo robot maps IDR to Hikvision's `requestKeyFrame` ISAPI call; `sim/video-source.sh`'s successor demonstrates LTR/intra-refresh with x264 (`intra-refresh=1` + keyframe-on-demand — the combination PROTOTYPE.md flagged as blocked on keyframe-on-demand).
**Measure:** time from loss to clean picture ≤ 1 RTT + 1 frame; join-to-first-frame < 150 ms on LAN.

### 1.3 Closed-loop rate control (ABR) inside the QoS ceiling
**Today:** profiles are static; `pilot-stats` is collected "for the deferred ABR loop". A hotspot's capacity varies 5× minute to minute.
**Build:** `seyd-qos::AbrController`, a pure function of 1 Hz samples: QUIC delivery rate, `rtt − min_rtt` (queue building), residual loss after FEC, backlog drops, pilot-reported true loss. Output: `{max_bitrate_kbps, fec_delta_pct, fec_key_pct, suggested_fps}` bounded by the profile. AIMD with latency gating (−25% on any of: backlog drop, `rtt−min_rtt > latency_budget/2`, residual loss > 0.5%; +10% after 3 clean seconds with headroom). FEC follows *measured* loss and is always paired with a bitrate cut (constant total budget — the rule SPEC.md established). Emit `on_requested_config` at most every 2 s, never for < 10% deltas. Multi-session: min over sessions. Sensor channels get a rate budget too (priority-ordered throttling under pressure).
**Measure:** trace tests (recorded netem/cellular traces → no oscillation, converges within 5 s); on the real 5G hotspot, glass-to-glass p95 stays under budget while bitrate tracks capacity.
**Status (2026-08-30):** implemented as `seyd-qos::abr::AbrController` + the engine's 1 Hz loop; the demo bridge applies `maxBitrateKbps` to the camera's VBR cap over ISAPI. Rules as built: FEC is burst-first (any measurable loss ≥0.2 % → k=2/25 %, ≥2 % → 38 %, ≥5 % → 50 %, key = delta+10 ≤ 50; down one level after 5 clean seconds) and paid for out of the video rate so the on-wire total stays at the profile budget; residual loss (≥2 genuinely incomplete frames/s, timed-out frames excluded) raises FEC before it ever cuts bitrate; AIMD −25 %/+10 % with backlog drops as the primary signal and a smoothed-RTT (median of 3, keyframe-polluted samples skipped, 2 consecutive seconds) latency gate; floor 25 % of ceiling, `suggestedFps: 15` after 5 s congested at the floor; bitrate emitted at most every 5 s for ≥10 % moves, +10 % after 5 clean seconds, FEC down a level after 10 clean seconds. Loss is measured skew-free (pilot `chunks_rx` against the chunks sent as of now−rtt−100 ms, sliding 3–5 s). First cellular run oscillated (24 changes/100 s) before these rules; after: 0 changes/90 s on a clean link, 3/90 s under a steady 3 % injected loss with FEC settled at 38/48.

### 1.4 Transport: BBR, pacing, PMTUD, larger datagrams, GSO
**Today:** aioquic's default CC and pacer; 1000-byte chunks chosen conservatively; per-frame `transmit()`.
**Build on quinn:** BBR + pacing for the media path (loss-based CC fills the modem buffer before reacting — bufferbloat *is* latency; cellular handover loss is not congestion). DPLPMTUD raises chunk payload toward ~1350 B (≈25% fewer packets, fewer FEC symbols per frame). Linux GSO/`sendmmsg` for keyframe bursts on Jetson/RPi. Admission control keeps Seyd's single-slot frame buffer *in front of* quinn with a small datagram send buffer (≈2 frames) so a backlog can never hide inside the stack — same semantics as `drop_threshold_bytes`, keyframes never dropped, whole-frame-or-nothing.
**Measure:** netem A/B BBR vs Cubic at 5–10% loss and 30 ms jitter: g2g p95, frames delivered; packets/frame before/after PMTUD.

### 1.5 Commands and sensors on the right channel type
**Today:** everything JSON on one bidi stream → a lost packet head-of-line-blocks PTZ and sensors behind it.
**Build:** channel kinds in the wire format: `COMMAND_UNRELIABLE` (continuous inputs: PTZ velocity, joystick — datagrams, per-channel sequence, latest-wins, sent at input rate) and `COMMAND_RELIABLE` (discrete: snapshot, mode changes — one QUIC stream per channel), same for sensors. Binary payloads (`octet-stream`/protobuf) with JSON only for control. Input timestamps carried so the robot can age out stale commands (the SDK exposes `max_command_age_ms`).
**Measure:** command RTT p99 under 5% loss ≤ 1.2× clean RTT.

### 1.6 Pilot: off-main-thread pipeline and true glass-to-glass
**Today:** transport read, reassembly, RS decode, `VideoDecoder`, and `ctx.drawImage` all on the main thread (`pilot.js:142-351`); frame timestamps are `performance.now()`.
**Build:** `@seyd/core` runs transport + reassembly + FEC + decode in a **Web Worker** and renders to an `OffscreenCanvas` (WebGL texture upload of the `VideoFrame`) so a customer's React app cannot jank video. `capture_ts` travels in the frame meta; a 1 Hz `ping/pong` on the control channel estimates clock offset; the HUD shows real glass-to-glass and per-stage breakdown (network, reassembly, decode queue, present). `decodeQueueSize > 1` is surfaced as a degraded signal (decoder falling behind → ABR `suggested_fps`).
**Measure:** main-thread busy-loop test (50 ms synthetic task every 100 ms) causes no frame-time regression; g2g reported within ±5 ms of an external camera measurement on LAN.

### 1.7 FEC: SIMD and burst-aware parity
**Today:** pure-Python `bytes.translate()` GF(256) — 0.25 ms/keyframe — but frame-shaped: latency-profile delta frames have k=2 and die to bursts of 3.
**Build:** `seyd-fec` with split-table SIMD (NEON/AVX2) — sub-50 µs keyframes; same Cauchy construction, must pass `tools/fec-vectors.py` byte-for-byte. Parity becomes per-block (§1.1) with a **burst-aware option**: interleave parity across the two most recent blocks (≤ one block of delay, ~4 ms at 3 Mbps, not a frame) once the cellular characterisation shows bursts matter. Parity % is driven by measured loss (§1.3); what FEC still misses is caught by §1.2 in one RTT — FEC no longer has to be sized for the worst case.
**Measure:** delivered % at 5%/burst-3 ≥ 97% (today ~90%) with total budget unchanged.

### 1.8 Reliability engineering
- **Network change:** re-gather candidates on interface/route change (netlink), renew port-mapping leases (today: 3600 s lease, never renewed), push updated candidates over signaling; pilot reconnects in < 1 s.
- **Cert rotation:** regenerate at day 10 of 13, overlap both certs for an hour, re-announce fingerprint.
- **Supervision:** watchdog on video ingest (RTSP stall → reopen), on session liveness (no pilot `pong` for 3 s → end session, park actuators — DEMO.md's park-on-release generalised into `on_session_ended`), on signaling (backoff reconnect, presence TTL).
- **Multi-session fan-out** (driver + observers) in the core, observers' delta frames dropped first under pressure.
- **Conformance and network emulation in CI:** FEC/wire vectors across Python→Rust→TS; `seydd` + `sim/` + headless pilot in Linux netns with `tc netem` (independent and `gemodel` burst loss, delay, jitter, rate caps); fuzzing of the wire and control parsers; a 24 h soak.
- **Cellular characterisation** (`tools/cellchar`): hours of per-chunk logs on a real 5G link → burst-length histograms → FEC/ABR defaults. PROTOTYPE.md's top "immediate" item; it makes §1.3 and §1.7 numbers real rather than guessed.

---

## Part 2 — Architecture

### 2.1 Monorepo layout

```
seyd/
├── Cargo.toml  package.json  pnpm-workspace.yaml
├── packages/                  # Seyd core — Rust
│   ├── seyd-wire/             # wire v2: chunk header, channel manifest, control messages
│   ├── seyd-fec/              # RS GF(256) Cauchy, SIMD; passes tools/fec-vectors
│   ├── seyd-qos/              # profiles + AbrController (pure, no I/O)
│   ├── seyd-nat/              # interfaces, STUN + classification, PCP/NAT-PMP/UPnP, lease renewal, NatReport
│   ├── seyd-transport/        # quinn endpoint; WebTransport (h3) + native QUIC (ALPN seyd/2); block sender w/ admission control; probing; cert rotation
│   ├── seyd-signal-client/    # WS client, signal v2, Ed25519 robot auth
│   ├── seyd-core/             # Agent lifecycle + engine: channels, sessions (multi), events
│   ├── seyd-ffi/              # C ABI (cdylib/staticlib), cbindgen → seyd.h
│   ├── seydd/                 # daemon: TOML config; RTP/RTSP (RFC 6184 depacketizer, retina) + UDP inputs; UDP command outputs
│   └── seyd-pilot-agent/      # (later) headless pilot over seyd-pilot-core → localhost RTP/UDP
├── sdks/                      # thin wrappers — NO protocol logic here
│   ├── c/  cpp/  python/  ros2/
│   └── js/core  js/web  js/react      # @seyd/core, <seyd-video>, <SeydVideo/>
├── cloud/api                  # TS Fastify+ws: /ws signal v2 + /api/v1; OIDC token verification (provider-agnostic); Postgres + Redis
├── cloud/prober               # cloud-side QUIC reachability probe of robot candidates
├── cloud/monitor              # synthetic pilot against the public demo robot
├── web/site (Astro landing, seyd.io)   web/console (React: login, fleet, robots, tokens; console.seyd.io)   web/demo (demo.seyd.io on @seyd/web)
├── docs/  (Starlight, docs.seyd.io + docs/adr/)   deploy/ (Terraform GCP, Dockerfiles)
├── examples/demo-robot/       # the Hikvision PTZ demo as a customer program on sdks/python (camera.py lands here)
├── tools/  sim/               # harnesses and robot stand-ins stay
└── legacy/agent-py/           # frozen Python agent until parity; then deleted
```

Moves: `camera.py` → `examples/demo-robot/hikvision.py` (closes CLAUDE.md's boundary exception); `pilot.js` logic → `sdks/js/core`, UI → `web/demo`; `fec.js` → `fec.ts` byte-identical; `packages/signal` → `cloud/api` with `relay-request`, `relay-mode`, binary forwarding, `cmd`/`cmd-out` **deleted**; `tools/relay-pilot.py` deleted; `demo.sh` → `examples/demo-robot/run.sh`. `SPEC.md`/`CLAUDE.md` rewritten under the Seyd name (Rust core, P2P-only with relay as future tier, new tree); `PROTOTYPE.md` frozen as history. Demo robot id becomes `seyd-demo`.

### 2.2 Rust core

**QUIC: quinn** — only Rust stack with a WebTransport server layer (`h3` + `h3-quinn` + `h3-webtransport`), pure Rust/rustls (trivial `cross` aarch64 builds and self-contained Python wheels), `AsyncUdpSocket` for pre-bound sockets (STUN on the QUIC socket + hole-punch probes, the ordering PROTOTYPE.md proved necessary), custom `ServerCertVerifier` for fingerprint pinning, DPLPMTUD, BBR/Cubic. quiche (BoringSSL FFI, no WT plumbing) and s2n-quic (no h3) rejected. Risk: `h3-webtransport` is experimental; the surface needed is tiny (extended CONNECT to `/seyd`, SETTINGS, datagram quarter-stream-id prefix) so an in-house `webtransport.rs` is the fallback. **First spike:** Chrome connects to quinn + h3 WT with `serverCertificateHashes` (`rcgen` ECDSA P-256, 13-day, SAN = candidate IPs).

**Two ALPNs, one socket:** `h3` (browser) and `seyd/2` (native: pilot agent later, agent↔agent). **Direction-agnostic QUIC** for native peers: split application role (robot/pilot) from transport role (server/client); `offer.direction: robot-listens | pilot-listens | both`, simultaneous open, first handshake wins. Designed into the wire/signal protocol now — the P2P-preserving answer for CGNAT robots once a native pilot exists.

**Prototype → crate mapping:** `fec.py` → `seyd-fec`/`seyd-wire`; `qos.py` → `seyd-qos`; `stun.py`, `portmap.py`, `agent.py::gather_candidates/bind_sockets` → `seyd-nat`; `cert.py`, `transport.py` → `seyd-transport`; `signaling.py` → `seyd-signal-client`; `peer.py::Relay` → `seyd-core`; `agent.py::PublisherControl` + CLI → `seydd`; `peer.py::_open_video` → `seydd::input`; `camera.py` → `examples/`; `pilot.js` race/reassembly/stats → `sdks/js/core`.

**C ABI (`sdks/c/include/seyd.h`) — sketch.** Shipped 2026-09-03 at
`SEYD_ABI_VERSION = 1`; `sdks/c/include/seyd.h` is the authority and ADR 0004's
"As built" section records where the shipped surface differs from this sketch
(the lifecycle moved into `seyd_core::agent::Agent`; `seyd_push_nal` waits for
per-block packing; `on_state`/`on_link_quality` are not implemented).
```c
seyd_status seyd_agent_create(const seyd_config*, const seyd_callbacks*, seyd_agent**);
seyd_status seyd_agent_start/stop(seyd_agent*);   void seyd_agent_destroy(seyd_agent*);
seyd_status seyd_channel_add(seyd_agent*, const seyd_channel_config*, seyd_channel_id*);
seyd_status seyd_push_nal  (seyd_agent*, seyd_channel_id, const uint8_t*, size_t, bool keyframe, bool end_of_frame, uint64_t capture_ts_us); /* sub-frame */
seyd_status seyd_push_frame(seyd_agent*, seyd_channel_id, const uint8_t* au, size_t, bool keyframe, uint64_t capture_ts_us);            /* convenience */
seyd_status seyd_push_message(seyd_agent*, seyd_channel_id, const uint8_t*, size_t);
seyd_status seyd_send_to_session(seyd_agent*, uint64_t session_id, seyd_channel_id, const uint8_t*, size_t);
seyd_status seyd_set_qos_profile(seyd_agent*, const char*);   seyd_status seyd_set_status_json(seyd_agent*, const char*);
```
- `seyd_config`: credential path / enrolment token, signal URL, QUIC port, ipv6 + port-mapping toggles, `qos_profile` (ceiling), `congestion`, `max_sessions`, `max_command_age_ms`.
- `seyd_channel_config`: `kind` ∈ VIDEO, SENSOR_{UN,}RELIABLE, COMMAND_{UN,}RELIABLE, BYTES_UDP; `name`, `codec`, `nominal_fps`, FEC override, `priority`.
- Callbacks (one Seyd thread, non-blocking): `on_state`, `on_session_started/ended` (park-on-release lives here), `on_command`, `on_link_quality` (state GOOD/DEGRADED/POOR/LOST, rtt, min_rtt, delivery rate, loss, fec_recovered, backlog drops), **`on_requested_config`** (`max_bitrate_kbps, latency_budget_ms, max_gop_ms, suggested_fps, reason`) — `PublisherControl.send()` generalised, the only place Seyd talks down to the encoder — **`on_recovery_request`** (`channel, kind: ltr|intra_refresh|idr, last_good_frame`), `on_nat_report`.
- Wrappers: `sdks/cpp/seyd.hpp` (RAII), `sdks/python` (`seyd` package, cffi ABI mode; wheels bundle `libseyd`; the demo robot uses this), `sdks/ros2/seyd_ros` (rclcpp component: `CompressedVideo`/`FFMPEGPacket` in, `GenericSubscription` for sensors, `GenericPublisher` for commands, `seyd_msgs/RequestedConfig` out; Humble + Jazzy).

**`seydd`** (`/etc/seyd/seydd.toml`): `[agent]` + `[[channel]]` blocks (`video` from `rtp://`/`rtsp://`, `sensor-*` from `udp://`, `command-*` to `udp://`, `bytes-udp` = Archetype B raw tunnel), `[publisher_control] udp=127.0.0.1:5003` keeping today's `video-config` JSON plus `suggestedFps`, `reason`, and a `recovery-request` message. Systemd unit, `.deb`, multi-arch Docker.

### 2.3 Wire protocol v2 (ADR 0001)
20-byte chunk header; v1 fields keep their meaning:
```
0  keyframe | fec_type | version=2      1  channel_id u8 (0 reserved)
2-3 frame_id u16 (per channel)          4-5 chunk_idx   6-7 n   8 k
9  flags2: bit0 frame_meta present, bit1 discardable, bit2 end_of_frame
10-11 last_len   12-13 chunk_len (PMTUD; fixed within a block)   14-17 send_ts u32 µs   18-19 block_idx
```
`n`/`k` describe the **FEC block** (§1.1); `block_idx` + `end_of_frame` let the receiver assemble a frame from blocks. Frame meta (chunk 0 of block 0): `capture_ts_us`, `seq_in_gop`. Control = one pilot-opened bidi stream, newline JSON: `hello{proto,client,token}` → `welcome{session_id, channels[], qos, clock}`; `ping/pong`; `loss{channel, frame_id, key}`; `request-keyframe`; `set-qos`; `agent-stats`/`pilot-stats`; `bye`. Reliable channels = one uni stream per channel per direction, varint-length-prefixed. Unreliable = datagrams with `n=1,k=0`. Session token in `hello`, verified by the agent (JWKS cached + pinned fallback, `aud == robot_id`, `exp`, `scope`) before any command channel opens. MoQ: not adopted (its value is relay fan-out — the deferred tier); the object model maps onto channel/GOP/frame so an adapter stays possible.

### 2.4 Web pilot SDK
`@seyd/core` (TS, zero deps; worker-hosted): `signal`, `transport` (WebTransport), `race` (priority sort, 400 ms `needsProbe` hold, `p2pHint` deadline, closes losers — load-bearing), `wire`, `fec`, `reassembler` (per-channel, per-block eager recovery, `MAX_REORDER`, decode-order gate, QoS close-out deadlines), `decoder` (codec from channel manifest, `optimizeForLatency`, `hardwareAcceleration:'no-preference'`, keyframe-required → `request-keyframe`), `clock`, `stats`, `session` (events `state, frame, sensor, link, stats, error, p2p-failed(NatReport)`), `diagnostics` (probe `wt-probe.seyd.io` to tell pilot-side UDP blocking from robot unreachability).
```ts
const s = new SeydSession({ signalUrl, token });
s.on('frame', ({channel, frame}) => …);     // or let <seyd-video> render it
s.send('drive', payload, { reliable: false });
await s.connect('robot-42');
```
`@seyd/web`: `<seyd-video>`, `<seyd-hud>`, `<seyd-connect-error>` (Shadow DOM custom elements). `@seyd/react`: `<SeydVideo/>`, `useSeydSession()`, `useSeydStats()`. The public demo page is the first consumer.

### 2.5 Cloud
- **Hosting (portable by construction):** Cloud Run in `europe-west1` today, but nothing may depend on it. `cloud/api` is a plain Docker image talking to **Postgres** and **Redis** over standard protocols — Cloud SQL and Memorystore are used only as managed instances of those, never through GCP-specific SDKs. No Firestore, Pub/Sub, Cloud Tasks, Secret Manager APIs or IAM-bound identities in application code: secrets arrive as environment variables, background jobs are containers on a cron, cross-instance routing is Redis pub/sub. `deploy/` is Terraform with the GCP bits isolated in one module and a `docker-compose.yml` that runs the whole cloud locally — the same compose file is the migration proof (Hetzner/OVH/Scaleway or Kubernetes anywhere). Images are pushed to GHCR as well as Artifact Registry. Redis presence + pub/sub removes `--max-instances 1`; `min-instances 1` kills cold starts. New GCP project under the Seyd name; `signal.seyd.io`.
- **Auth (provider-agnostic; ADR 0007, decided 2026-09-04):** authentication is a pluggable edge, authorization is the product. `cloud/api` verifies **OIDC access tokens via the provider's JWKS** — issuer and audience are config, nothing else. (Access, not ID: an API's audience is its resource indicator. Access tokens often omit `email`, so `OidcAuthenticator` falls back to userinfo once per user.) Users are keyed on `(oidc_issuer, oidc_subject)`, so changing provider is an `UPDATE` of two columns matched on verified email. **Logto 1.43.0 self-hosted** is the provider — chosen on footprint, not features: since orgs, roles and grants are ours, the right provider is the smallest one that federates email+password, social and later enterprise SSO behind a *single issuer*. Pinned, never `latest`; it has an active advisory stream and no SCIM (Ory Polis is the bolt-on if an enterprise demands it). The console uses Authorization Code + PKCE; the login *page* is the provider's, branded in its admin console — a redirect flow does not give us our own form. Three provisioning policies cover the deployment shapes: `self-serve`, `invite-only`, `default-org` (`docs/self-hosting-auth.md`). API keys are Seyd's own authenticator and stay enabled whatever the provider. Session JWTs for robots are minted by Seyd itself (ES256, our keys, published at `/.well-known/seyd-session-jwks.json`) — they never depend on the identity provider, so a fleet keeps working while it is down.
- **Data model:** `orgs`, `users(oidc_issuer, oidc_subject, email)`, `org_memberships(role)`, `robots(public_key, tags, config, last_seen_at)`, `enrolment_tokens`, `api_keys(scopes)`, `robot_grants(scope drive|observe)`, `sessions(outcome, path_label, direction, alpn, nat_report, pilot_diag, failure_reason, metrics)`, `session_events`, `demo_requests`. Redis `presence:{robot_id}` TTL 20 s.
- **Robot identity:** enrolment token → robot generates Ed25519 key → `POST /enrol` → credential file; WS auth is challenge-response. **Pilot session:** `POST /session-tokens {robot_id, scope, ttl≤300 s}` → ES256 JWT `{iss: seyd.io, aud: robot_id, scope, exp, jti}`; revocation via `session-revoked`.
- **Signal v2:** R→S `auth`, `announce{candidates, cert_fingerprints[], alpns, nat_report, channels, p2p_hint, max_sessions}`, `heartbeat`, `session-accepted/ended`, `report`; P→S `auth`, `connect{robot_id, client, diag}`, `abort`, `report`; S→P `offer{…, direction, nat_report, prober_result, channels}`, `queued`, `observer`, `robot-offline`, `denied`, `peer-disconnected`; S→R `challenge`, `pilot-connecting{session_id, pilot_ip, token_claims, direction}`, `punch`, `session-revoked`; console `subscribe-presence`/`presence`. Gone: relay-*, cmd/cmd-out, qos-over-signal, binary frames.
- **Console:** login/sign-up → fleet (online/offline/in-session, custom status, NAT badge) → robot detail (candidates, NatReport, prober result, sessions, "open pilot" mints a token) → enrolment tokens → API keys → sessions + metrics.
- **Observability:** OTel from `cloud/api` and `seydd`; dashboards: p2p_success_rate by nat_type × candidate label, TTFF, g2g p50/p95, residual loss, ABR levels. `failure_reason` closed enum (`no-candidates`, `all-candidates-timeout`, `cert-mismatch`, `token-rejected`, `pilot-udp-blocked`, `robot-offline`, `handshake-timeout`).

### 2.6 P2P-failure UX
Agent `NatReport` in `announce` (refreshed on network change): ipv4 `{local, public, nat, cgnat, gateway_external}`, ipv6 `{present, global, inbound_ok}`, portmap `{protocol, external, error, lease_s}`, per-candidate results, `prober{reachable[], unreachable[]}`, `hint`. `cloud/prober` sends a QUIC Initial to each candidate so reachability is *measured* before any pilot connects and shown as a badge at enrolment. `<seyd-connect-error>` picks copy by class, each linking `docs.seyd.io/networking/<class>`:

| Class | Guidance |
|---|---|
| pilot UDP blocked | Your network blocks UDP/QUIC (corporate Wi-Fi/VPN); try another network or allow outbound UDP 443/4433. The robot is reachable. |
| robot CGNAT, no portmap, no IPv6 | Direct browser connection impossible. Enable IPv6 on the SIM/APN; SIM with public IP; put the robot behind a PCP/UPnP router. |
| symmetric NAT (not CGNAT) | Enable UPnP/NAT-PMP/PCP or forward UDP {port} → {local ip}. |
| port-restricted + portmap error | Port mapping failed ({error}); enable UPnP or forward UDP {port}. |
| portmap ok, prober unreachable | Upstream firewall / double NAT; forward UDP {port} on the outer router too. |
| IPv6 present, inbound blocked | Open a pinhole for UDP {port} on the IPv6 firewall. |
| cert / token mismatch | Stale robot cert or token; reconnect. |

### 2.7 Landing page and public demo
`web/site` (Astro, static; served from the same Cloud Run/nginx container pattern as the console so it moves with everything else — a CDN in front is optional and must stay optional; `seyd.io` with `seydio.com` redirecting; a self-hostable captcha such as Altcha/Turnstile on the form): `/` hero with live demo embed + request-a-demo, `/how-it-works` (single encode chain, no jitter buffer, P2P, FEC — the measured numbers), `/demo`, `/request-demo` (→ `POST /api/v1/demo-requests` → Postgres + email + Slack), `/docs`, `/pricing` placeholder, `/security`. Demo on the new stack: `examples/demo-robot` (Python SDK over `libseyd`, Hikvision driver, keyframe/recovery via ISAPI), driver + observer sessions with a 90 s driver slot and queue, PTZ ignored from observers, per-IP slot limits, PTZ rate cap, auto-home on session end, RTSP-stall watchdog, `cloud/monitor` synthetic pilot every 5 min → Slack, camera DHCP reservation. `seyd-demo` has a public `observe/drive` grant; every other robot needs a token.

---

## Part 3 — Ordered work

**Field test, run A (2026-08-30, pilot on an iPhone hotspot, robot on the office
LAN):** hole punch worked (`srflx`, Telia mobile → cone NAT), RTT 22 ms, 25 fps
at ~1.7 Mbps, g2g p50 17 ms, 0.0 % true chunk loss, no keyframes lost, PTZ
responsive; the owner judged the experience good. Two findings: (1) the
reassembler's LAN-tuned silence deadline (30 ms) closed out 33 jittered frames
as "lost" and triggered 18 keyframe requests — fixed with an adaptive
deadline (p95 intra-frame gap × 4 + 10, ≤ 250 ms) and a recovery cadence of one
per half GOP; (2) g2g p95 (101 ms) is dominated by 30–60 KB IDRs serialising
over a ~2 Mbps uplink — the case for intra-refresh/LTR (§1.2) and for ABR
(§1.3) on cellular. **The recovery ladder landed 2026-09-05** — `request_recovery`
climbs `ltr` → `intra_refresh` → `idr` instead of always demanding a keyframe,
with the profile's `recovery_grace_ms` releasing the sender when a publisher
recovers without one. Remaining latency work, ordered and reasoned:
`docs/latency-roadmap.md`. Run B (robot on the hotspot) is still to do.

**Status (2026-08-28, later):** Milestone A steps 0–9 have a first working
implementation: `seyd-fec`, `seyd-wire`, `seyd-qos`, `seyd-nat`,
`seyd-transport` (quinn + h3-webtransport, verified with real Chrome),
`seyd-signal-client`, `seyd-core` (engine, packer), `seydd` (RTSP via retina,
in-house RFC 6184 RTP, UDP sensors/commands, publisher control), `cloud/api`
(signal v2, dev-mode auth, presence, static hosting), `@seyd/core` +
`@seyd/web` + `web/demo` (worker-hosted pilot), and `examples/demo-robot`
(seydd config + Hikvision bridge outside core). `tools/seyd-smoke.py` drives
the demo page in headless Chrome against the real stack and passes on the
`sim/` source, including with 5 % injected loss. Legacy Python/JS code deleted after the camera run. Since done (2026-09-01): cert
rotation with fingerprint overlap, port-mapping lease renewal, network-change
re-gather, and the cloud prober (`seyd-prober` on Cloud Run; `inbound_ok` and
an honest, punch-aware `p2p_hint` on every announce). Since done (2026-09-02):
reconnect backoff resets after a healthy session in both `seyd-signal-client`
and `seydd`'s RTSP input — a session that stayed up ≥ 30 s starts its next
backoff at 1 s instead of inheriting the escalation, so a robot up for hours
recovers from a blip in a second rather than up to 30. Found on a 1 h 50 m
demo-camera run where both the RTSP and signal connections reset every 10–17
minutes (a local network path issue, not Seyd) and the backoff never returned
to its floor.

**Since done (2026-09-03): the C ABI and the Python SDK** (step 5's remainder).
The agent lifecycle moved out of `seydd`'s `main()` into
`seyd_core::agent::Agent`, so `seydd` and `seyd-ffi` are both thin hosts of one
lifecycle rather than one of them owning it — see ADR 0004, "As built".
`seyd-ffi` is a cdylib/staticlib at `SEYD_ABI_VERSION = 1`; `sdks/c/include/seyd.h`
is cbindgen output, checked in, with CI failing if it drifts from the crate.
`sdks/python` is cffi in ABI mode and reads its declarations *out of that
header* rather than retyping them. Verified end to end: a Python program
holding its own x264 output pushed access units through the C ABI and
`tools/seyd-smoke.py` passed against it in headless Chrome — 30 fps, 1730 kbps,
g2g p50 0.79 ms, zero true loss, PTZ commands round-tripping to the robot.
`seyd_push_nal` is deliberately absent from ABI 1 (the engine still packs per
frame; §1.1 is Milestone B), and adding it later is an append, not a break.
**Also absent from ABI 1: enrolment.** `seyd_config` has a credential path but
no enrolment token, so an SDK robot cannot redeem a token itself; `seydd enrol`
is the only robot-side path today, and `py-robot.sh` shells out to it against
the SDK's credential file (verified 2026-09-08 against the deployed cloud with
real accounts). Enrolment belongs in `seyd_core::Agent`, with `seydd` and
`seyd-ffi` both hosting it — an append to the ABI, not a break.

Not yet done from Milestone A: PMTUD-driven `chunk_len`, the console UI
(login/sign-up), `sdks/cpp` and `sdks/ros2`, Jetson/RPi builds and the wheel
matrix, netem CI. The repository directory/remote rename and DNS are owner
actions still pending.

The signal server is deployed: `https://seyd-signal-flj7s44j4a-ew.a.run.app`
(project `seydio`, `europe-west1`, dev-mode auth), serving the demo page at `/`.
`./demo-seyd.sh` starts the camera robot against it; verified with
`tools/seyd-smoke.py` on the real Hikvision camera (25 fps 1280×720, PTZ moves
the camera). **Finding:** from a public HTTPS origin Chrome blocks the `host`
(private-IP) candidate with `ERR_BLOCKED_BY_LOCAL_NETWORK_ACCESS_CHECKS`
unless the user grants the Local Network Access permission prompt, so a
same-LAN pilot falls through to the `srflx` hairpin (12 ms instead of <1 ms
g2g). Headed browsers show the prompt; `<seyd-connect-error>`/the HUD should
explain it (add to §2.6 classes) — pending.

### Milestone A — the new stack runs the demo
0. **Rename.** Repo → `seyd`; `CLAUDE.md`/`SPEC.md`/`DEMO.md` rewritten under the Seyd name (product decisions above folded in); new GCP project (`europe-west1`); DNS for `seyd.io`, `seydio.com` (redirect), `signal.`/`console.`/`demo.`/`docs.`/`wt-probe.` subdomains; npm scope `@seyd`, PyPI name `seyd`, crate prefix reserved. Legacy Python/relay code untouched.
1. ADRs: 0001 wire v2, 0002 quinn, 0003 MoQ position, 0004 C ABI. Workspace skeleton (Cargo + pnpm), CI (`rust.yml`, `js.yml`).
2. `seyd-fec` + `seyd-wire` passing `tools/fec-vectors.py` (Rust check added beside `fec-check.js`).
3. WebTransport spike: quinn + h3 WT server, Chrome connects with `serverCertificateHashes`; decide vendored vs in-house `webtransport.rs`.
4. `seyd-nat` (port of stun/portmap/candidates + lease renewal + netlink re-gather + NatReport), `seyd-transport` (block sender with admission control, BBR, PMTUD, probing, cert rotation), `seyd-signal-client`.
5. `seyd-core` (the `Agent` lifecycle, channels, multi-session fan-out, control stream, token verification, watchdogs), `seyd-ffi` + `seyd.h`, `sdks/python`. **Done 2026-09-03** except the manylinux wheel matrix, which waits on the cross builds in step 10.
6. `seydd` with RFC 6184 RTP + `retina` RTSP inputs (sub-frame pipelining), UDP sensor/command channels, publisher-control UDP sink.
7. `@seyd/core` (worker + OffscreenCanvas, per-block reassembly, clock sync, g2g HUD, diagnostics), `@seyd/web`, `@seyd/react`; `web/demo` built on them.
8. `cloud/api` v2 (OIDC JWKS verification with configurable issuer, Postgres, enrolment, session tokens, prober hook) + `docker-compose.yml` running api, Postgres and Logto locally; `web/console` (PKCE login/sign-up, fleet, robot detail, members, keys, audit). Portability check: the full cloud runs from compose on a laptop with no GCP credentials. **Done 2026-09-04 except Redis** — presence and sessions are still in `MemoryStore`, so the service is single-instance; that work is independent of identity. Verified on 2026-09-04 against the real compose stack (Colima on macOS): migrations apply, `PgAccounts` serves sign-up/enrolment/grants/audit, org-scoped presence isolates two customers, `seydd enrol` redeems a token into Postgres, and Logto 1.43.0 serves OIDC discovery and JWKS to both the browser and the API container. The one step still unexercised is the browser redirect flow, which needs Logto's first-run admin setup (cloud/README.md).
9. `examples/demo-robot` on `sdks/python` + Hikvision driver with `requestKeyFrame` on recovery request; driver/observer policy; `cloud/monitor`.
10. `tools/pilot-smoke.py` (Playwright) and `tools/e2e` netem suite pass against `seydd` + `web/demo`; Jetson/RPi 5 build via `cross`. Then delete `legacy/agent-py`, relay code, `relay-pilot.py`, and the old `darc-signal` Cloud Run service.

### Milestone B — make it fast and provably reliable
11. `seyd-qos::AbrController` + sensor rate budgets; `on_requested_config` end-to-end with `sim/`'s successor (live-reconfigurable x264 publisher) and the camera.
12. NACK-driven recovery: pilot `loss` messages → `on_recovery_request` (LTR / intra-refresh / IDR); publisher example with x264 intra-refresh + keyframe-on-demand.
13. `tools/cellchar` on a real 5G link → FEC defaults, burst-aware interleaving decision, ABR trace fixtures.
14. Command/sensor channel kinds on datagrams with input timestamps; PTZ moves to `COMMAND_UNRELIABLE`.
15. SIMD FEC, GSO, perf pass on RK3588/Jetson (CPU per 1080p30 stream < 3%); fuzzing of wire/control parsers; 24 h soak.
16. **Touch controls for the pilot** (before the landing page invites phone visitors). Android Chrome on a handset is a confirmed supported platform (verified 2026-09-02), but only pan and tilt work by touch: `web/demo/src/ptz.ts` drives the virtual joystick from pointer events, while everything else is mouse- or keyboard-only. Missing on touch: **zoom** (wheel or `+`/`-` only — a phone operator cannot zoom at all), **recentre** (`H` only), **the fine/fast speed modifier** (`Shift` only, so touch gets one speed curve), and **the HUD toggle** (`S`). Pinch-to-zoom needs real multi-pointer tracking, not just a gesture listener — today a second `pointerdown` overwrites `pointerVec` and `setPointerCapture` is taken for a single pointer. Decide what belongs in `@seyd/web` as default on-screen affordances versus what stays demo-specific in `web/demo`; the SDK ships the video element, so a customer building a mobile operator page should not have to reimplement zoom. Verify on a real handset, not a desktop emulator.
17. Landing page live with demo-request flow; docs site with networking guides for every failure class; `.deb` + apt repo (`apt.seyd.io`), Python wheels (manylinux x86_64/aarch64), npm packages, Docker images.
18. **EU-migration rehearsal** (before the first security-review customer): stand the cloud up on one EU provider (Scaleway for like-for-like managed Postgres/Redis, or Elastx/Cleura for the Swedish story) and run the smoke tests against it. Timebox one day — if it takes longer, that is a portability bug to fix. Decision context and move triggers: docs/eu-hosting.md.

### Milestone C — breadth (when customers pull it)
19. `sdks/ros2/seyd_ros` (Humble/Jazzy) and `sdks/cpp`.
20. `seyd-pilot-core` + `seyd-pilot-agent` (native `seyd/2`, direction-agnostic, localhost RTP/UDP front end for Archetype B).
21. iOS (`SeydKit`, UniFFI + VideoToolbox), Android (UniFFI + MediaCodec), Flutter — on customer demand, **and the only route to iPhone/iPad**: the web pilot is Chromium-only (open decision 5), so no iOS device can run it. A native app is not bound by WebKit and keeps fingerprint pinning and the candidate race exactly as they are, which is why this is the mobile answer rather than the CA-signed-cert fallback. Android needs no native SDK to be reachable — the web pilot runs in Chrome on a handset today (verified 2026-09-02) — so iOS is the one that closes a real gap; the Android SDK is for customers who want a native app, not for access.
22. Relay tier, fast: a QUIC-forwarding relay with a public UDP address (the WebSocket relay of ADR 0010 is the one that *exists*; this is the one that is fast), live upgrade from relay to direct (the prototype retried P2P every 30 s while relaying; the new engine needs the agent to accept a second `hello` for a session it already serves), FEC off over the relay, metering into the console; MPQUIC bonding evaluation; session recording.

---

## Verification

- **Milestone A:** `python3 tools/fec-vectors.py | cargo run -p seyd-fec --example check` and `| node tools/fec-check.js` both pass; `tools/pilot-smoke.py --expect-ptz` passes against `seydd` + `web/demo` on LAN and broadband with winning candidate `host`/`portmap`; join-to-first-frame < 150 ms on LAN (keyframe on `hello`); two browsers show driver/observer roles, PTZ ignored from the observer; a forged session token is refused before any command is delivered; sign-up in the console (via the pluggable OIDC provider) creates an org, an enrolment token enrols a robot, and it appears online; `docker compose up` in `cloud/` brings up the whole cloud with no Google dependency and the same smoke tests pass against it; stopping the demo robot fires the monitor alert; netem at 5% independent loss ≥ 99% delivery and 5%/burst-3 ≥ 90% (no regression vs PROTOTYPE.md's table); `seydd` runs on a Jetson or RPi 5; no "darc" identifiers remain outside `legacy/` and git history.
- **Milestone B:** ABR trace tests converge without oscillation; on the 5G hotspot g2g p95 stays under the profile budget as bitrate tracks capacity; loss-to-clean-picture ≤ 1 RTT + 1 frame with LTR/intra-refresh, ≤ 1 RTT + 1 IDR with the camera; 5%/burst-3 delivery ≥ 97%; command RTT p99 under 5% loss ≤ 1.2× clean; main-thread jank test shows no frame-time regression; g2g reading within ±5 ms of an external camera measurement; 24 h soak with no restart; fuzzers clean.
- **Always:** SPEC.md/CLAUDE.md describe what the code does (the doc rule in CLAUDE.md); every measured number in this plan is re-measured and written into the docs when the corresponding work lands.

---

## Owner decisions still open
1. ~~**Identity provider**~~ — **decided 2026-09-04: Logto 1.43.0, self-hosted (ADR 0007).** Still open within that: whether Logto's corporate domicile survives a European customer's security questionnaire (Zitadel, Swiss, is the alternative and the seam makes it a config change), that Logto publishes no DPA, and that there is no mail transport in the cloud yet — so invitation tokens are handed over by whoever issued them rather than emailed.
2. Repo visibility / licence (open SDKs + `seyd-fec`/`seyd-wire`, closed core?).
3. Regions and residency: `europe-west1` only for now; whether session metrics/NAT reports may leave the EU if a US signaling region is added later; whether to move off GCP entirely to an EU provider (the compose stack keeps that a deployment task, not a rewrite).
4. Pricing model (SPEC open question 3) — needed for the console's `plan` field and the relay tier.
5. ~~Safari: verify `serverCertificateHashes`~~ — **verified 2026-09-02: Safari does not support it, so the web pilot is Chromium-only and no iOS device can run it** (every iOS browser is WebKit). Decided for now: do **not** build the CA-signed-cert fallback (`<robot>.p2p.seyd.io` + DNS-01) — it would force candidate URLs to become hostnames, likely losing the LAN `host` candidate to DNS-rebinding protection and putting DNS TTLs in front of network-change re-gather, for a control-plane dependency we currently don't have. Mobile is served by the Milestone C native SDKs (item 21) instead; Android already works today in Chrome on a handset (verified 2026-09-02). Hardened on 2026-09-03: WebKit has said it *does not intend* to implement `serverCertificateHashes` (`w3c/webtransport#623`, closed as not planned), so this is a permanent constraint, not a wait. A WASM module cannot route around it — the page has no UDP socket to run QUIC on, and the verifier sits below the API. Still open: whether a customer need reopens the browser-on-iOS question, and if so, WebRTC data channels (fingerprint trust by design, ICE for free, but a second transport and its own ADR) versus CA-signed certs — where Let's Encrypt IP-address certs (GA 2026-01, 6-day profile) are the unevaluated variant that keeps candidates as IP literals but only serves publicly reachable, stably addressed robots. Per-robot certs with robot-generated keys either way (never one shared wildcard key).
6. GStreamer pipeline input in `seydd` beside RTP/RTSP.
7. First design partner and archetype.
