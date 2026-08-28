# Signal protocol v2

WebSocket, path `/ws`, JSON text frames, every message has `"type"`. The first
message from a client is `auth`. The server has three client roles: `robot`,
`pilot`, `console`. There is no relay: the cloud never carries media.

## Robot

| dir | type | fields |
|---|---|---|
| S→R | `challenge` | `nonce` (base64, 32 bytes) — sent on connect |
| R→S | `auth` | `v: 2`, `role: "robot"`, `robot_id`, `public_key` (base64 Ed25519, 32 bytes), `sig` (base64 Ed25519 signature over the raw nonce bytes), `agent_version` |
| S→R | `auth-ok` | `robot_id` |
| S→R | `denied` | `reason` — then close |
| R→S | `announce` | `candidates: [{url, label, priority, needs_probe, family: 4\|6}]`, `cert_fingerprints: [hex sha256]`, `alpns: ["h3"]`, `nat_report`, `channels: [{id, kind, name, codec, fps}]`, `p2p_hint: "likely"\|"lan-only"\|"none"`, `max_sessions` — sent after `auth-ok` and again whenever anything in it changes |
| R→S | `heartbeat` | `sessions: [session_id]`, `status?` (free-form object shown in the console) — every 5 s |
| S→R | `pilot-connecting` | `session_id`, `pilot_ip`, `role: "driver"\|"observer"`, `direction: "robot-listens"` |
| S→R | `punch` | `session_id`, `pilot_ip` — pilot is retrying; reopen the NAT hole without touching existing sessions |
| S→R | `session-revoked` | `session_id`, `reason` — also how a robot learns a pilot left: `pilot-disconnected`, `pilot-abort`, `revoked` |
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
| S→P | `offer` | `session_id`, `robot_id`, `role`, `candidates`, `cert_fingerprints`, `p2p_hint`, `direction`, `nat_report`, `channels` |
| S→P | `robot-offline` | `robot_id` |
| S→P | `denied` | `reason` |
| P→S | `retry` | `session_id` — asks the cloud to send `punch` to the robot |
| P→S | `abort` | `session_id` |
| P→S | `report` | `session_id`, `outcome: "p2p"\|"failed"`, `failure_reason?`, `path_label?`, `metrics?` |
| S→P | `peer-disconnected` | `session_id` |

Failure reasons (closed set): `no-candidates`, `all-candidates-timeout`,
`cert-mismatch`, `token-rejected`, `pilot-udp-blocked`, `robot-offline`,
`handshake-timeout`.

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
* Roles are fixed at `offer` time. When the driver leaves, existing observers
  are **not** promoted; the next new `connect` becomes driver.
* `connect` before the robot has sent `announce` → `robot-offline`.
* A full robot (`max_sessions` reached) → `denied {reason: "robot-busy"}`.
* A robot reconnecting supersedes its previous socket: the old one is closed
  and its sessions ended.
* `offer.channels` is the robot's last `announce`; `welcome.channels` on the
  control stream is authoritative and replaces it.
* Console `auth` outside dev mode is refused (`token-rejected`) until OIDC lands.

## Console

| dir | type | fields |
|---|---|---|
| C→S | `auth` | `v: 2`, `role: "console"`, `token?` |
| C→S | `subscribe-presence` | — |
| S→C | `presence` | `robots: [{robot_id, online, channels, p2p_hint, nat_report, sessions: [{session_id, role, subject}], status, last_seen}]` — full list on subscribe, then on every change |

## HTTP

`GET /healthz` → `200 ok`. `GET /api/v1/robots` → the presence list (dev
mode: unauthenticated).
