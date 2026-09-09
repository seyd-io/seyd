# Signal protocol v2

WebSocket, path `/ws`, JSON text frames, every message has `"type"`. The first
message from a client is `auth`. The server has three client roles: `robot`,
`pilot`, `console`. Media never travels on this socket. When no direct path
connects, a session can be carried by the relay on `/relay` (ADR 0010, below);
that is a separate socket per party per session.

## Robot

| dir | type | fields |
|---|---|---|
| S→R | `challenge` | `nonce` (base64, 32 bytes) — sent on connect |
| R→S | `auth` | `v: 2`, `role: "robot"`, `robot_id`, `public_key` (base64 Ed25519, 32 bytes), `sig` (base64 Ed25519 signature over the raw nonce bytes), `agent_version` |
| S→R | `auth-ok` | `robot_id` |
| S→R | `denied` | `reason` — then close |
| R→S | `announce` | `candidates: [{url, label, priority, needs_probe, family: 4\|6}]`, `cert_fingerprints: [hex sha256]`, `alpns: ["h3"]`, `nat_report`, `channels: [{id, kind, name, codec, fps}]`, `p2p_hint: "likely"\|"lan-only"\|"none"`, `max_sessions`, `relay: bool` (will serve a relayed session; absent = false) — sent after `auth-ok` and again whenever anything in it changes |
| R→S | `heartbeat` | `sessions: [session_id]`, `status?` (free-form object shown in the console) — every 5 s |
| S→R | `pilot-connecting` | `session_id`, `pilot_ip`, `role: "driver"\|"observer"`, `direction: "robot-listens"` |
| S→R | `punch` | `session_id`, `pilot_ip` — pilot is retrying; reopen the NAT hole without touching existing sessions |
| S→R | `session-revoked` | `session_id`, `reason` — also how a robot learns a pilot left: `pilot-disconnected`, `pilot-abort`, `revoked`, `relay-closed` |
| S→R | `relay-open` | `session_id`, `url`, `token`, `role`, `pilot_ip` — the pilot's race failed and it is waiting on the relay; dial `url`, attach with `token` (ADR 0010) |
| R→S | `session-accepted` | `session_id`, `path_label` |
| R→S | `session-ended` | `session_id`, `reason` |

Robot identity: the cloud stores `public_key` per `robot_id`. In **dev mode**
(`SEYD_DEV_OPEN_ENROLMENT=1`) an unknown `robot_id` is enrolled on first
sight (trust on first use); a known `robot_id` with a different key is denied.
Production enrolment tokens are PLAN.md §2.5 and not part of this milestone.

## Pilot

| dir | type | fields |
|---|---|---|
| P→S | `auth` | `v: 2`, `role: "pilot"`, `token?` |
| S→P | `auth-ok` | `subject` |
| P→S | `connect` | `robot_id`, `client: {kind, alpn: "h3"}` |
| S→P | `offer` | `session_id`, `robot_id`, `role`, `candidates`, `cert_fingerprints`, `p2p_hint`, `direction`, `nat_report`, `channels`, `relay: {url, token} \| null` — present only when the robot announced `relay` and the server allows it |
| S→P | `robot-offline` | `robot_id` |
| S→P | `denied` | `reason` |
| P→S | `retry` | `session_id` — asks the cloud to send `punch` to the robot |
| P→S | `abort` | `session_id` |
| P→S | `report` | `session_id`, `outcome: "p2p"\|"relay"\|"failed"`, `failure_reason?`, `path_label?`, `metrics?` |
| S→P | `peer-disconnected` | `session_id` |

Failure reasons (closed set): `no-candidates`, `all-candidates-timeout`,
`cert-mismatch`, `token-rejected`, `pilot-udp-blocked`, `robot-offline`,
`handshake-timeout`, `relay-unavailable` (the race failed *and* the relay
could not be attached or dropped).

Tokens: in dev mode a pilot without a token is accepted as `subject:
"anonymous-<n>"`. The demo robot is public. Production session tokens are
PLAN.md §2.5.

## Sessions and roles

The cloud assigns one **driver** per robot (first come; the driver slot is
released on disconnect/abort/`session-ended`) and any number of **observers**
up to the robot's `max_sessions`. The role is fixed at `offer` time and
repeated in `pilot-connecting` so the agent can ignore commands from observers.
A `session_id` is a random 16-hex-char string; the pilot presents it in the
control-stream `hello`.

Clarifications (from the first implementation):
* **Reachability annotation:** when a prober is configured
  (`SEYD_PROBER_URL`), the cloud dials each publicly-probeable announced
  candidate with a real QUIC handshake after every `announce` (rate-limited
  per robot). The stored `nat_report` is then annotated — `prober = {from:
  "cloud", reachable: [labels], unreachable: [labels], ts}`, per-candidate
  `candidates[].ok`, `ipv6.inbound_ok` when a `host6` candidate was probed —
  and `p2p_hint` is recomputed honestly (any candidate reachable → `likely`;
  all probeable candidates dark → `none`). Offers and presence carry the
  annotated report. Private/link-local/CGNAT addresses are never probed
  (`ok: null`).
* Roles are fixed at `offer` time. When the driver leaves, existing observers
  are **not** promoted; the next new `connect` becomes driver.
* `connect` before the robot has sent `announce` → `robot-offline`.
* A full robot (`max_sessions` reached) → `denied {reason: "robot-busy"}`.
* A robot reconnecting supersedes its previous socket: the old one is closed
  and its sessions ended.
* `offer.channels` is the robot's last `announce`; `welcome.channels` on the
  control stream is authoritative and replaces it.
* Console `auth` outside dev mode is refused (`token-rejected`) until OIDC lands.

## Relay (ADR 0010)

Path `/relay`, one WebSocket per party per session, opened by the **pilot
first** and only after its candidate race failed. The server never looks
inside frames.

| dir | message | notes |
|---|---|---|
| P→S, R→S | text `relay-attach {session_id, token, party}` | first message; `token` is `offer.relay.token` (pilot) or `relay-open.token` (robot) |
| S→P, S→R | text `relay-attached {session_id}` | both sockets are paired; the pilot may now send `hello` |
| S→P, S→R | text `denied {reason}` then close | `unknown-session`, `bad-token`, `relay-busy`, `relay-unavailable` (robot offline, or it did not attach within 10 s) |
| S→P, S→R | text `relay-closed {session_id, reason}` then close | the session ended for any reason |
| both ways | binary `[kind, …payload]` | `kind 1`: one datagram, the bytes `send_datagram` would have sent; `kind 2`: a segment of the control stream (NDJSON, any split) |

Ordering: the pilot attaches → the server sends the robot `relay-open` → the
robot attaches → the server sends both `relay-attached`. Closing either socket
ends the session (`session-revoked {reason: "relay-closed"}` to the robot,
`peer-disconnected` to the pilot). A datagram frame for a socket with more than
256 KB buffered is dropped; control frames are never dropped. The server logs
`relay: session ended` with bytes each way and seconds, for metering. Server
switch: `SEYD_RELAY=0` disables the relay (offers carry `relay: null`);
`SEYD_RELAY_URL` overrides the URL derived from the request's host.

## Console

| dir | type | fields |
|---|---|---|
| C→S | `auth` | `v: 2`, `role: "console"`, `token?` |
| C→S | `subscribe-presence` | — |
| S→C | `presence` | `robots: [{robot_id, online, channels, p2p_hint, nat_report, sessions: [{session_id, role, subject}], status, last_seen}]` — full list on subscribe, then on every change |

## HTTP

`GET /healthz` → `200 ok`. `GET /api/v1/robots` → the presence list (dev
mode: unauthenticated).
