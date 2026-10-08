# ADR 0010 — A cloud relay as the last resort, never a silent fallback

**Status:** accepted 2026-09-09. Reopens the "P2P only, no relay fallback"
decision of 2026-08-28 (PLAN.md, SPEC.md, CLAUDE.md), at the owner's request.

## Context

Field runs A and B (docs/field-test.md) and the demos since have shown the
same thing: a robot behind a consumer 4G/5G router, CGNAT or a locked-down
office network cannot be reached directly from a browser, and the pilot's
diagnosis — correct as it is — leaves a demo audience looking at a red box.
The direct path stays the product; what changed is that "fail with a
diagnosis" was costing demos, and the owner asked for a relay through the
existing cloud when, and only when, no direct path connects.

Two constraints shape the design:

* **Cloud Run takes TCP only.** The signal server runs on Cloud Run
  (`europe-west1`, one instance). A QUIC-forwarding relay with a public UDP
  address — the design SPEC.md sketched for the relay tier — cannot run there.
  A WebSocket relay on the signal server can, with no new service, no new
  address and no new secret.
* **The pilot's transport is not negotiable per session.** The browser
  reaches the robot with WebTransport and nothing else; the relay has to look
  like a transport to the SDK, and to the agent, so that neither the
  reassembler, FEC, the control stream, admission control nor ABR need a
  second implementation.

## Decision

1. **The relay is a second backend of the same transport, on both ends.**
   `seyd_transport::Session` is either a WebTransport session or a relay
   session; the engine handles both through one API and learns which it has
   from `Session::kind()`. In the SDK, `Transport` has a `WebTransportTransport`
   and a `RelayTransport`; the engine's pipeline runs unchanged over either.
   Nothing above the transport knows the difference — except that it is told.

2. **One WebSocket per party per session, to `/relay` on the signal server.**
   The first message is a text `relay-attach {session_id, token}`; after
   `relay-attached`, every frame is binary: byte 0 is the kind (`1` datagram,
   `2` control-stream bytes) and the rest is the payload, exactly the bytes
   that would have been a QUIC datagram or a control-stream segment. The
   server pairs the two sockets by session id and forwards frames verbatim; it
   never parses media. The pilot's token comes in the `offer` (`offer.relay =
   {url, token}`), the robot's in `relay-open`, which the cloud sends only
   once the pilot has actually attached. Each token admits exactly one party
   of one session.

3. **The relay is taken last, and only when offered.** The pilot attaches
   after its candidate race has failed (`no-candidates`,
   `all-candidates-timeout`, `handshake-timeout`, …) and only if the offer
   carried a relay, which the cloud includes only when the robot announced
   `relay: true` and the server has `SEYD_RELAY` enabled. `seydd.toml
   [agent] relay = false`, `SeydSessionOptions.relay = false` and
   `<seyd-video relay="0">` each refuse it. There is no relay-first mode and no
   retry-through-the-relay: the race runs first, every time.

4. **A relayed session is always shown as one.** `welcome`, `stats` and the
   session carry `transport: 'relay'`; the pilot reports `outcome: "relay"`;
   the agent labels the path `relay`; the HUD's path line reads *RELAY via
   cloud* in amber with the direct-path failure reason next to it; `<seyd-video>`
   keeps an amber RELAY badge up for the whole session; and
   `<seyd-connect-error>` keeps the network guidance visible, headed "Relayed
   through the Seyd cloud", because the relay is the symptom of a network
   problem, not its cure. A customer must never mistake a relayed session for
   the product's latency.

5. **Backpressure and loss keep their meaning.** On the robot, the relay
   session counts the datagram bytes it has queued and not yet written, so
   `send_buffer_queued()` and the profile's drop threshold work as over QUIC;
   a congested leg shows up as backlog and drops delta frames, exactly as ADR
   0005/0009 assume. The relay's WebSocket ping gives ABR an RTT for the
   robot's leg. On the server, a frame to a socket with more than 256 KB
   buffered is dropped if it is a datagram and queued if it is control, so a
   slow pilot costs frames rather than the session. FEC parity still travels
   (the sender does not know the transport) and is wasted on TCP; that is
   bandwidth, not latency, and is left for a later change.

## Consequences

* **Measured on loopback, balanced profile, synthetic source (2026-09-09):**
  the relayed session ran at 30 fps, 3.3 Mbps, 0 % loss, with PTZ commands and
  sensor data crossing it; the tools/seyd-smoke.py assertions pass with
  `?paths=none` (race fails instantly, relay takes over) and without it (direct
  path, `path=host`). Bytes through the server: 4.76 MB in 11 s to the pilot,
  5 KB to the robot.
* **Latency is worse and not yet measured off-loopback.** TCP head-of-line
  blocking on loss, a second hop, and Cloud Run's front end all add delay.
  Re-measure with tools/latency-ab.py through tools/link-shaper.py before
  quoting any relay number; until then the relay is "works", not "fast".
* **Cost is real and metered.** Every relayed byte crosses the server twice
  (in and out); the outbound half is billed egress. At the balanced profile
  (~26 MB per minute to the pilot) that is on the order of 0.2–0.4 US cents
  per relayed minute in `europe-west1` at list price, plus the instance's
  CPU-minute while any session is live (~0.15 cents). `seyd-business/business.md` keeps
  the relay a separately priced tier for this reason; the per-session byte
  counts are logged (`relay: session ended`) for metering.
* **Sessions cap at Cloud Run's request timeout.** Deploys now set
  `--timeout 3600`, the platform maximum; a relayed session older than an hour
  drops and reconnects (the pilot re-races and re-relays). A self-hosted cloud
  (`cloud/docker-compose.yml` in `seyd-cloud`) has no such limit.
* **No live upgrade to direct.** The prototype retried P2P every 30 s while
  relaying and switched over live. The new engine does not yet: a relayed
  session stays relayed until it ends. Reload to retry. Adding it needs the
  agent to accept a second `hello` for a signal session it is already
  serving, which is a protocol change of its own.
* **Announce and offer changed** (docs/protocol/signal-v2.md): `announce.relay`,
  `offer.relay`, `relay-open`, `relay-attach`, `relay-attached`,
  `relay-closed`; failure reason `relay-unavailable`; report outcome `relay`.
  An agent that predates this ADR announces no `relay`, and its pilots are
  offered none.
* **Superseded text.** PLAN.md's "P2P only. No relay fallback", SPEC.md's
  "Media never touches Seyd's servers" and CLAUDE.md's fixed decision are
  amended: media touches Seyd's servers only on the relay path, which the
  pilot opts into last and sees always. The QUIC-forwarding relay with its own
  address remains the design for a relay that must be fast; this one is the
  relay that must exist.
