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
├── web/theme                  # @seyd/theme: the design system as code — tokens, base styles, fonts, light/dark switch (docs/design.md)
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

**One design system for everything a person sees (decided 2026-09-16, `docs/design.md`).** The demo, the console, the SDK overlays and the presentations share one set of tokens, three typefaces and four colour meanings (green = direct/online/primary, amber = relayed/in use/attention, red = stop, the rest neutral), in light and dark. `web/theme` (`@seyd/theme`) is the system as code: product surfaces import it and keep only layout in their own stylesheets; the SDK overlays read the same `--seyd-*` tokens from the host page with fallbacks, so a customer page that never heard of the theme still looks right. Fonts are bundled, never fetched from a CDN, because Seyd runs self-hosted and offline and a visitor's address must not leak for a typeface.

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
**As built (2026-09-19): the landing page is `web/demo/index.html`**, served at
`/` by seyd-signal together with the pilot page and the console. It is the
demo page grown into a landing page: the hero carries the live robot list
(the demo is the proof), and the sections below it are the pitch deck and the
*Inside the Seyd Engine / Cloud* walkthroughs condensed for a visitor — the
problem, how it works, the latency stack, loss handling, reachability and the
relay as the shown last resort, integration (the real `seydd.toml` and
`<seyd-video>` shapes), use cases, the measured field numbers with the span
each covers, cloud and identity, the tiers without prices, and the ordered
roadmap. Layout only in the page; tokens and components from `@seyd/theme`
(docs/design.md); no request-a-demo form yet because there is no endpoint or
mailbox for it. Every number on the page must be re-measured when the code it
describes changes (CLAUDE.md), and the roadmap column must move with PLAN.md.

Still planned, when a customer or a launch pulls it: a separate `web/site`
(Astro, static; same container pattern as the console so it moves with
everything else — a CDN in front is optional and must stay optional; `seyd.io`
with `seydio.com` redirecting; a self-hostable captcha such as Altcha/Turnstile
on the form) with `/how-it-works`, `/docs`, `/pricing`, `/security` and
`/request-demo` (→ `POST /api/v1/demo-requests` → Postgres + email + Slack).
Demo on the new stack: `examples/demo-robot` (Python SDK over `libseyd`, Hikvision driver, keyframe/recovery via ISAPI), driver + observer sessions with a 90 s driver slot and queue, PTZ ignored from observers, per-IP slot limits, PTZ rate cap, auto-home on session end, RTSP-stall watchdog, `cloud/monitor` synthetic pilot every 5 min → Slack, camera DHCP reservation. `seyd-demo` has a public `observe/drive` grant; every other robot needs a token.

### 2.8 Developer documentation (decided 2026-09-21)

**The docs are generated from the code wherever the code can say it, and the
hand-written parts import the code rather than quote it.** A guide that
pastes a snippet drifts within a week; a guide that renders
`sdks/c/examples/sensor-robot.c` cannot. The rule for every page: if a fact
can be produced by a generator, it is; if an example can be a real file that
the existing checks compile or run, it is; only prose is typed by hand.

**Site.** `web/docs` — Astro + Starlight (Markdown/MDX, sidebar, self-hosted
Pagefind search, light and dark), themed with `@seyd/theme` tokens and its
bundled fonts, so it looks like the landing page and the console and loads
nothing from a CDN. Built with `base: /docs/` and bundled into the seyd-signal
image at `/docs/` exactly like the console (`cloud/api/deploy.sh`), so it moves
with everything else; `docs.seyd.io` is a DNS entry pointing at the same
service when the owner sets it up. Every generated file lives under a path
that is gitignored and rebuilt by `pnpm --filter docs build`; a fresh checkout
needs only the toolchain the rest of the repo already needs (Rust, Python 3,
Node).

**What is tied to code, and how (`tools/docs/`):**

| Surface | Source of truth | Generator | Best practice it follows |
|---|---|---|---|
| C ABI reference | `sdks/c/include/seyd.h` (cbindgen output; its doc comments are the Rust `///` comments in `seyd-ffi`) | `gen-c-reference.py` parses the header into one page per group: status codes, config, callbacks, functions, counters | The header is the documentation, as for any C library; comments are written once, in Rust |
| Python reference | `sdks/python/seyd/agent.py` docstrings and `#:` attribute comments | `gen-python-reference.py` walks the AST (no import, so no library needed) | Docstrings are the API docs (PEP 257); the page is what Sphinx autodoc would produce |
| JavaScript reference | TSDoc on the exports of `@seyd/core` and `@seyd/web` | `starlight-typedoc` (TypeDoc + Markdown) at build | TypeDoc is the TypeScript standard; the sidebar is generated from the exports |
| Rust reference | `///` on every workspace crate | `cargo doc --workspace --no-deps`, copied to `/docs/rust/` | rustdoc, with doctests compiled by `cargo test` |
| `seydd.toml` reference | `packages/seydd/src/config.rs` structs, their `///` comments and serde defaults | `gen-seydd-config.py` | The config struct is the schema |
| QoS profiles | `packages/seyd-qos/src/lib.rs` `LATENCY`/`BALANCED`/`QUALITY` constants | `gen-qos-profiles.py` | One table, regenerated when a number changes |
| Networking guidance | the `FailureClass` union in `sdks/js/web/src/seyd-connect-error.ts`, whose "How to fix this" link is `docs.seyd.io/networking/<class>` | `check-networking.py` fails the build if a class has no page | The SDK's links can never dangle |
| Protocol contracts, ADRs, encoder setup, Starlink, self-hosted identity | `docs/protocol/*.md`, `docs/adr/*.md`, `docs/*.md` | `collect.mjs` copies them in with front matter derived from the first heading | One source; the site is a view of the repo |
| Examples | `sdks/c/examples/*.c` (built by `make -C sdks/c`), `sdks/python/examples/*.py`, `sdks/js/core/examples/*.ts` (type-checked by the docs build), `sdks/js/web/examples/*.html`, `examples/demo-robot/seydd.toml` | MDX imports the file with `?raw` and renders it | The example is the file; the check that compiles it is the check that the doc is right |
| The integration skill (decided 2026-10-06) | `skills/seyd/`: `SKILL.md` (the procedure: read the user's code, interview for the rest, choose the form factor, write the plan, implement, verify) and `references/` (one file per part; `seydd-config.md` and `qos-profiles.md` rendered by the generators above) | `gen-skill.py` regenerates the two references; `--check` fails the build if a C function or callback, Python method or handler, `<seyd-video>` attribute, `SeydSession` event or failure reason, `AgentEvent` variant, publisher-control message or failure class exists in the code and is not mentioned, if a named path or `/docs/` route is dead, or if a generated file is stale. `generate.mjs` publishes the folder verbatim at `/docs/skill/` with a `files.txt` manifest | Agent Skills format (`SKILL.md` + references), so Claude Code and other agents load it unchanged; the developer's agent interviews for what the plan needs instead of guessing. The page is *Integrate with a coding agent* under *Start here* |

**Hand-written pages (prose only, examples imported):**

- *Start here* — what Seyd is (with the three-party figure), concepts (agent,
  channels, sessions and roles, QoS profiles, keyframes on demand, direct
  first and the relay), choosing a form factor (daemon vs SDK vs Rust), and a
  five-minute quickstart that drives the public demo from a page of your own.
- *Robot side* — integrating the daemon (install, the config file, enrolment,
  a systemd unit, the publisher-control loop, verifying), a robot in Python, a
  robot in C, a robot in Rust, the publisher contract (what `video-config`,
  `recovery-request` and `layer` ask of an encoder), simulcast.
- *Pilot side* — the web components, building your own UI on `SeydSession`,
  reading the HUD and what "g2g" measures (docs/latency-sources.md §0).
- *Networking* — reachability overview and one page per failure class, each
  with the exact router or SIM change, plus Starlink.
- *Cloud* — enrolment and access (tokens, grants, session passes, the
  console), running the cloud yourself (compose), identity providers.
- *Reference* — the generated pages above, the protocol contracts, the ADRs.

**Figures** (`web/docs/src/components/figures/`, inline SVG on the design
tokens so they follow the theme): the three parties; the agent lifecycle
(create → channels → start → sessions → stop); the frame pipeline from camera
to canvas; the channel model (video and sensors robot→pilot, commands
driver→robot, observers read-only); the daemon's place on the robot (camera
RTSP/RTP in, UDP sinks and sources, publisher control out); the candidate race
and the relay decision; enrolment and the three identity planes; the
publisher-control loop.

**Keeping it current (CLAUDE.md carries the rule):** a change to the C header,
the Python package, the `@seyd/core`/`@seyd/web` exports, `seydd`'s config
struct, the QoS constants or the failure classes is not done until
`pnpm --filter docs build` passes, the guide that explains the changed
surface says the new thing, and `skills/seyd/` says what an integrator does
with it (the build's `gen-skill.py --check` catches the mechanical part). A new example is a file under an SDK's
`examples/`, never a code block in a page.

---

## Part 3 — Ordered work

**Open-sourcing (decided 2026-10-06):** `docs/open-source.md` is the ordered
work for publishing the core, the SDKs, the web pilot, the docs and the
examples at `github.com/seyd-io/seyd` under Apache-2.0 (copyright Anton
Gravestam, DCO for contributions), with the hosted cloud, the console, the
prober and the deploy tooling in a private `seyd-io/seyd-cloud` that pins the
public repo as a submodule, and the business plan, pricing, customers and
competitors in a documents-only private `seyd-io/seyd-business`. History is
filtered, not squashed. Publishing the
packages (npm, PyPI, crates.io) follows it.

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

Not yet done from Milestone A: PMTUD-driven `chunk_len`, `sdks/cpp` and `sdks/ros2`, Jetson/RPi builds and the wheel
matrix, netem CI. The repository directory/remote rename and DNS are owner
actions still pending.

The signal server is deployed: `https://seyd-signal-flj7s44j4a-ew.a.run.app`
(project `seydio`, `europe-west1`, dev-mode auth), serving the demo page at `/`.
Until a domain is owned, `https://seydio.web.app` is the shareable address: a
redirect-only Firebase Hosting site (`deploy/firebase-redirect/`) that 302s
every path to that URL; no application code depends on Firebase.
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
8. `cloud/api` v2 (OIDC JWKS verification with configurable issuer, Postgres, enrolment, session tokens, prober hook) + `docker-compose.yml` running api, Postgres and Logto locally; `web/console` (PKCE login/sign-up, fleet, robot detail, members, keys, audit). Portability check: the full cloud runs from compose on a laptop with no GCP credentials. **Done 2026-09-04 except Redis** — presence and sessions are still in `MemoryStore`, so the service is single-instance; that work is independent of identity. Verified on 2026-09-04 against the real compose stack (Colima on macOS): migrations apply, `PgAccounts` serves sign-up/enrolment/grants/audit, org-scoped presence isolates two customers, `seydd enrol` redeems a token into Postgres, and Logto 1.43.0 serves OIDC discovery and JWKS to both the browser and the API container. The one step still unexercised is the browser redirect flow, which needs Logto's first-run admin setup (cloud/README.md). **Console deployed 2026-09-10** (ADR 0007 amendment): Logto on Cloud Run (`cloud/logto/`, own Neon database, admin console not public), the console bundled into seyd-signal at `/console/`, invite-only in both senses — registration closed at the provider, an invitation carries the provider's one-time sign-in token and is one link. Provider settings reach the console from `GET /api/v1/console-config`, so one build serves any deployment. Still no mail transport: Logto's HTTP email connector posts to the API, which logs; a password reset code shows up in the log. The first human login (setting a password) is the owner's to do.
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

### Milestone B½ — developer documentation (§2.8)
Ordered: the site and its theme; the generators (C, Python, seydd config, QoS, networking check) and the TypeDoc and rustdoc integration; the daemon guide and the three language guides with their examples imported; the pilot guides; the networking pages; the cloud pages; the figures; the landing page links; deploy at `/docs/`. Then `docs.seyd.io` DNS (owner), and a REST reference for `/api/v1` generated from the route table (not yet).

### Milestone C — breadth (when customers pull it)
19. `sdks/ros2/seyd_ros` (Humble/Jazzy) and `sdks/cpp`.
20. `seyd-pilot-core` + `seyd-pilot-agent` (native `seyd/2`, direction-agnostic, localhost RTP/UDP front end for Archetype B).
21. iOS (`SeydKit`, UniFFI + VideoToolbox), Android (UniFFI + MediaCodec), Flutter — on customer demand, **and the only route to iPhone/iPad**: the web pilot is Chromium-only (open decision 5), so no iOS device can run it. A native app is not bound by WebKit and keeps fingerprint pinning and the candidate race exactly as they are, which is why this is the mobile answer rather than the CA-signed-cert fallback. Android needs no native SDK to be reachable — the web pilot runs in Chrome on a handset today (verified 2026-09-02) — so iOS is the one that closes a real gap; the Android SDK is for customers who want a native app, not for access.
22. Relay tier, fast: a QUIC-forwarding relay with a public UDP address (the WebSocket relay of ADR 0010 is the one that *exists*; this is the one that is fast), live upgrade from relay to direct (the prototype retried P2P every 30 s while relaying; the new engine needs the agent to accept a second `hello` for a session it already serves), FEC off over the relay, metering into the console; MPQUIC bonding evaluation; session recording.
23. **Signaling continuity — sessions must survive the signaling socket, and deploys must be invisible** (found 2026-09-09, deferred by the owner: "fine for now"). Two facts drive this. (a) Cloud Run treats every WebSocket as one HTTP request bounded by the service's request timeout (300 s until 2026-09-09, 3600 s — the platform maximum — since, set for relayed sessions). At the deadline the socket is cut regardless of activity; every client reconnects within ~1 s. But the hub cannot tell a pilot whose socket timed out from one who closed the tab: it ends the session and sends `session-revoked`, and the robot closes a perfectly healthy QUIC session on that. So a direct session lives at most as long as the pilot's signaling socket (now 60 min), then re-races after the 15 s retry; a relayed session is cut at its own 60 min as well and does not reconnect. (b) On a redeploy, sockets stay on the *draining* old revision until they close — up to that same hour — while presence is per-instance memory. Pilots in a session are unaffected until their socket dies; new pilots get `robot-offline` until the robot's old socket dies or the robot is restarted (observed 2026-09-09; CLAUDE.md now says restart the robot after every deploy). `deploy.sh` makes two revisions per run, so two drains. The work, cheapest first: **(i)** a grace period in the hub before a dropped pilot or robot socket ends its sessions, and `auth` carrying the session ids the client believes are live so a reconnect *resumes* them — the robot is never told to revoke a session whose pilot merely reconnected; the relay socket gets the same reconnect-and-resume so a relayed session outlives the hour; **(ii)** the server closes every signaling socket itself every few minutes with a "reconnect" close code, so after a deploy robots and pilots converge on the newest revision within that interval instead of within an hour — invisible once (i) exists; **(iii)** the Redis presence + sessions of §2.5 (the part of item 8 still open), which makes every revision see the same robots and sessions and is the proper fix for both (a) and (b). Verify with a session recorded through a forced socket cut (`SEYD_WS_MAX_AGE_MS` or a deploy) showing no `session-revoked` and no gap in frames.

### Milestone D — what the Tello flights taught (2026-10-02/03, DEMO-TELLO.md)

Five flights across three network setups — the same room, the drone behind a
4G router with the pilot behind 5G, and a session from the agent's own
laptop — with the drone flown out to its 100 m range limit. Seyd's own share
held in every one: 30 ms agent-to-display, the direct path across the two
mobile routers through PCP port mapping at 61–74 ms round trip, one-frame
repairs after loss. Everything that went wrong happened at the edges Seyd
does not cover yet. Ordered by how much each would have changed those
flights:

24. **Publisher-link health into the agent.** Loss is measured on the pilot
    leg only (ADR 0006), so when the drone's own radio tore a picture a
    second and, at range, delivered 1 keyframe in 80 requested, the HUD said
    "no loss" and the rate controller kept asking for 3 Mbps. A publisher
    reports what it sees of its *own* source link — torn pictures per
    second, keyframes requested versus delivered, source throughput — over a
    publisher-to-agent message (publisher control is one-way today); the
    controller lowers its request when the source is the bottleneck, and the
    HUD shows "source link" and "Seyd link" as two things, because a pilot
    reacts differently to each. Applies to every camera behind its own
    wireless hop, which is most field robots. The bridge-local version —
    lower the drone's encoder level when torn pictures climb, so keyframes
    shrink and survive — exists in both Tello hosts since 2026-10-06 and is
    the stopgap; this item moves that judgment into the agent, fed by a
    signal every publisher can send.
25. **Driver presence in the protocol, not in every demo page.** A
    hands-off hover landed after 5 s, a reconnect raised a false "pilot
    gone" from a stale timestamp, and a 5 s 5G stall at 100 m landed the
    drone where it was — three bugs from one presence rule each bridge
    invents for itself. The agent already knows whether the driver's
    transport is alive, when they last sent anything, and (via the SDK)
    whether their tab is visible. Standard `driver-idle` / `driver-gone`
    events to the robot with a configurable timeout, in `seydd.toml` and
    the C ABI, make the safety behaviour one tested thing.
26. **Session continuity across a reload.** A page reload is a new
    session; the robot sees "last driver left" and the drone lands. A grace
    period in which the same pilot resumes their session, keeping the driver
    role and the robot's state, removes that class of surprise. Session ids
    exist; this is policy on top of them, and it is the pilot-side twin of
    item 23's signaling-socket resume.
27. **A notice channel from robot to pilot.** "Take-off refused, battery
    13 %" and "landing, no pilot" were smuggled into telemetry JSON and
    special-cased on the page. One standard operational-notice message,
    rendered by the HUD, serves every robot.
28. **The robot's profile is the session's default.** The range flight ran
    on `balanced` because the page defaults to it, while the robot's config
    said `latency` and a weak drone link wanted it. The robot already
    announces its ceiling; the session starts there and the pilot adjusts
    within it.
29. **Keyframe survivability on lossy links.** At range the drone delivered
    deltas but not keyframes: 14 datagrams against 9. The drone's link, but
    the same arithmetic applies to Seyd's own path on a bad connection. The
    key-frame FEC rate exists; raising it when keyframes are failing, or
    spreading a keyframe over a longer pacing window, is to be measured with
    `tools/latency-ab.py` and the link shaper.
30. **Flight analysis as a product surface.** Every conclusion above came
    from parsing a text log by hand. Per-session timelines in the console —
    item 24's source health beside the pilot-leg statistics the HUD already
    has — are the session recording of item 22 made useful to a customer.

---

## Verification

- **Milestone A:** `python3 tools/fec-vectors.py | cargo run -p seyd-fec --example check` and `| node tools/fec-check.js` both pass; `tools/pilot-smoke.py --expect-ptz` passes against `seydd` + `web/demo` on LAN and broadband with winning candidate `host`/`portmap`; join-to-first-frame < 150 ms on LAN (keyframe on `hello`); two browsers show driver/observer roles, PTZ ignored from the observer; a forged session token is refused before any command is delivered; sign-up in the console (via the pluggable OIDC provider) creates an org, an enrolment token enrols a robot, and it appears online; `docker compose up` in `cloud/` brings up the whole cloud with no Google dependency and the same smoke tests pass against it; stopping the demo robot fires the monitor alert; netem at 5% independent loss ≥ 99% delivery and 5%/burst-3 ≥ 90% (no regression vs PROTOTYPE.md's table); `seydd` runs on a Jetson or RPi 5; no "darc" identifiers remain outside `legacy/` and git history.
- **Milestone B:** ABR trace tests converge without oscillation; on the 5G hotspot g2g p95 stays under the profile budget as bitrate tracks capacity; loss-to-clean-picture ≤ 1 RTT + 1 frame with LTR/intra-refresh, ≤ 1 RTT + 1 IDR with the camera; 5%/burst-3 delivery ≥ 97%; command RTT p99 under 5% loss ≤ 1.2× clean; main-thread jank test shows no frame-time regression; g2g reading within ±5 ms of an external camera measurement; 24 h soak with no restart; fuzzers clean.
- **Always:** SPEC.md/CLAUDE.md describe what the code does (the doc rule in CLAUDE.md); every measured number in this plan is re-measured and written into the docs when the corresponding work lands.

---

## Owner decisions still open
1. ~~**Identity provider**~~ — **decided 2026-09-04: Logto 1.43.0, self-hosted (ADR 0007).** Still open within that: whether Logto's corporate domicile survives a European customer's security questionnaire (Zitadel, Swiss, is the alternative and the seam makes it a config change), that Logto publishes no DPA, and that there is no mail transport in the cloud yet — invitations are links handed over by whoever issued them (2026-09-10: they now carry the provider's one-time sign-in token, so no account has to exist first), and a password-reset code lands in the seyd-signal log via the HTTP email connector until a transport exists. Candidate: an SMTP relay or an EU provider's API behind `MailSink` (`cloud/api/src/mail.ts`), sending from the seyd.io domain once DNS is set up.
2. Repo visibility / licence (open SDKs + `seyd-fec`/`seyd-wire`, closed core?).
3. Regions and residency: `europe-west1` only for now; whether session metrics/NAT reports may leave the EU if a US signaling region is added later; whether to move off GCP entirely to an EU provider (the compose stack keeps that a deployment task, not a rewrite).
4. Pricing model (SPEC open question 3) — needed for the console's `plan` field and the relay tier.
5. ~~Safari: verify `serverCertificateHashes`~~ — **verified 2026-09-02: Safari does not support it, so the web pilot is Chromium-only and no iOS device can run it** (every iOS browser is WebKit). Decided for now: do **not** build the CA-signed-cert fallback (`<robot>.p2p.seyd.io` + DNS-01) — it would force candidate URLs to become hostnames, likely losing the LAN `host` candidate to DNS-rebinding protection and putting DNS TTLs in front of network-change re-gather, for a control-plane dependency we currently don't have. Mobile is served by the Milestone C native SDKs (item 21) instead; Android already works today in Chrome on a handset (verified 2026-09-02). Hardened on 2026-09-03: WebKit has said it *does not intend* to implement `serverCertificateHashes` (`w3c/webtransport#623`, closed as not planned), so this is a permanent constraint, not a wait. A WASM module cannot route around it — the page has no UDP socket to run QUIC on, and the verifier sits below the API. Still open: whether a customer need reopens the browser-on-iOS question, and if so, WebRTC data channels (fingerprint trust by design, ICE for free, but a second transport and its own ADR) versus CA-signed certs — where Let's Encrypt IP-address certs (GA 2026-01, 6-day profile) are the unevaluated variant that keeps candidates as IP literals but only serves publicly reachable, stably addressed robots. Per-robot certs with robot-generated keys either way (never one shared wildcard key).
6. GStreamer pipeline input in `seydd` beside RTP/RTSP.
7. First design partner and archetype.
