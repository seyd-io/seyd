# seydd — daemon configuration and robot-side interfaces

`seydd --config /etc/seyd/seydd.toml`

```toml
[agent]
robot_id       = "seyd-demo"
signal_url     = "wss://signal.seyd.io/ws"
credential_path = "/var/lib/seyd/robot.key"   # Ed25519 seed, created on first run
quic_port      = 4433
ipv6           = true
port_mapping   = true
qos_profile    = "balanced"                    # the ceiling
max_sessions   = 4

[[channel]]
kind  = "video"
name  = "main"
input = "rtsp://user:pass@192.168.1.20:554/Streaming/Channels/101"   # or "rtp://0.0.0.0:5000"
codec = "avc1.42001f"
fps   = 25

[[channel]]
kind   = "sensor"
name   = "telemetry"
input  = "udp://127.0.0.1:5002"
codec  = "json"

[[channel]]
kind   = "command"
name   = "ptz"
output = "udp://127.0.0.1:5004"
codec  = "json"

[publisher_control]
udp = "127.0.0.1:5003"
```

Secrets: credentials in RTSP URLs may also come from the environment as
`SEYD_RTSP_USER` / `SEYD_RTSP_PASSWORD`, which are substituted into an
`rtsp://` URL that has no userinfo. URLs are redacted in every log line.

## Robot-side UDP interfaces (generic — nothing here knows any vendor)

**Command output** (`[[channel]] kind="command" output=`): each command
message received from the driver is written as one UDP datagram containing
the raw payload (for `codec="json"` that is the JSON text). Commands from
observers are dropped. The daemon prepends nothing.

**Sensor input** (`kind="sensor" input=`): each UDP datagram received becomes
one sensor message to every connected pilot.

**Publisher control** (`[publisher_control] udp=`): best-effort JSON, one
object per datagram, fire-and-forget:

```json
{"type": "video-config", "channel": 1, "profile": "balanced",
 "maxBitrateKbps": 3000, "latencyBudgetMs": 100, "maxGopMs": 1000,
 "suggestedFps": 0, "reason": "profile"}
{"type": "video-config", "channel": 1, "profile": "balanced",
 "maxBitrateKbps": 2250, "latencyBudgetMs": 100, "maxGopMs": 1000,
 "suggestedFps": 0, "reason": "abr-down"}
{"type": "recovery-request", "channel": 1, "kind": "idr", "reason": "pilot-loss"}
{"type": "session", "state": "started"|"ended", "session_id": "…", "role": "driver"|"observer", "sessions": 1}
```

`video-config` is sent once at start (`reason: "profile"`), whenever the QoS
profile changes (`"pilot-request"`), and whenever the closed-loop controller
(PLAN.md §1.3, `seyd-qos::abr`) moves the request inside the ceiling
(`"abr-down"`, `"abr-up"`). `maxBitrateKbps` is the *current* request — never
above the profile ceiling, never below 25 % of it; `suggestedFps` is non-zero
only when the request is at the floor and the link is still congested. Seyd's
own FEC rates move with it and are visible in `agent-stats` as
`abr_bitrate_kbps`, `abr_ceiling_kbps`, `abr_fec_delta`, `abr_fec_key`,
`abr_reason` (`steady|loss|latency|backlog|residual|recover|fec-down`).

`recovery-request` is sent when a pilot reports an unrecoverable keyframe or
asks for a keyframe (`request-keyframe`), rate-limited to one per 250 ms per
channel. `session` lets robot-side code park actuators when the last driver
leaves. A robot whose publisher ignores all of this keeps streaming on whatever
it was configured with.
