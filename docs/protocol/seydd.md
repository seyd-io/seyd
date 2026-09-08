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
codec = "avc1.42001f"                          # or "hev1.1.6.L93.B0" for H.265
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

## Enrolment

A robot joins a fleet by redeeming a one-time token created in the console
(Fleet → *Create enrolment token*). Either hand it to the daemon and let it
enrol on first start —

```toml
[agent]
enrolment_token = "seyd_enr_…"     # or SEYD_ENROLMENT_TOKEN, which wins
```

```
SEYD_ENROLMENT_TOKEN=seyd_enr_… seydd --config /etc/seyd/seydd.toml
```

— or do it as a separate step, for an interactive install:

```
seydd --config /etc/seyd/seydd.toml enrol --token seyd_enr_…
```

Either form creates `credential_path` if it does not exist, then sends the
**public** half of the key with the token to `POST /api/v1/enrol`, at the
address derived from `signal_url` (`wss://host/ws` → `https://host`; override
with `--signal-url`). The private seed never leaves the robot, and no user
credential is involved: the robot has no account and never contacts the
identity provider, so enrolled robots keep connecting while it is down
(ADR 0007).

Because it registers whatever key the robot already holds, enrolment also works
on a robot that predates it — one that joined a dev server by
trust-on-first-use keeps its identity and simply becomes known. On a first run
a failure is fatal; with a credential already present it is only a warning and
the daemon carries on, since a single-use token left in the config will not
redeem twice and must not stop an enrolled robot from starting.

A token is single-use: a second robot presenting it gets `410` and needs a new
one. Prefer `SEYD_ENROLMENT_TOKEN` in a provisioning script, so the secret need
not be written to disk.

**When the signal server denies the robot:** `unknown-robot` means it was never
enrolled — redeem a token as above. `key-mismatch` means the id is enrolled
with a *different* key: another agent is using it, or the credential file was
replaced. Delete the robot in the console and enrol again.

In development, `SEYD_DEV_OPEN_ENROLMENT=1` on the signal server trusts an
unknown robot on first connection instead, which is how `sim-robot.sh` and the
smoke harness work. Such a robot has no org and is visible only to an
unauthenticated console.

## Video codecs

`codec` is a WebCodecs identifier, and it does double duty: seydd uses it to
pick the RTP framing, and it travels unchanged to the browser's decoder.

| Codec | `codec` value | Ingest | Browser |
|---|---|---|---|
| H.264 | `avc1.…` | RTSP and RTP | Everywhere |
| H.265 | `hev1.…` / `hvc1.…` | RTSP and RTP | **Only where the machine has a hardware HEVC decoder** |
| Motion JPEG | `mjpeg` | **RTSP only** | Everywhere, via `ImageDecoder` |

Seyd never decodes video, so the codec matters in exactly two places: finding
frame boundaries in RTP, and knowing which access units are random access
points (H.264 IDR; H.265 IRAP, types 16–23 — narrowing that to IDR alone would
miss the CRA pictures several camera encoders emit). Chunking, FEC, transport
and pacing are all bytes.

**Motion JPEG** is the compatibility path. ONVIF Profile S makes it the one
*mandatory* codec while H.265 is merely conditional, so every conformant camera
can emit it and none is obliged to emit anything better. It costs about an
order of magnitude more bandwidth — measured here, 640×480@15 MJPEG used
3478 kbps against 3431 kbps for H.264 at 1280×720@30, the same link for an
eighth of the pixels — so `seydd` warns when a channel uses it. Every frame is
a complete JPEG and therefore a keyframe, which makes the FEC, drop and
recovery paths strictly simpler.

It is **RTSP only**. RFC 2435 strips the JPEG headers from every packet and the
receiver has to rebuild a JFIF header from the type, Q and dimensions; retina
does that on the RTSP path, and a bare `rtp://` MJPEG channel is refused with
an explanation rather than silently delivering nothing. The same RFC also
requires *standard* Huffman tables — a source using optimised ones cannot be
packetised at all, which is why `ffmpeg` needs `-huffman default` when
generating a test stream.

In the browser MJPEG bypasses WebCodecs entirely: it is not in the codec
registry, so `@seyd/core` decodes each frame with `createImageBitmap` and wraps
it in a `VideoFrame`, leaving the presenter, canvas and stats paths unchanged.

The H.265 caveat is real and worth testing before promising it to a customer:
measured on Chrome 152, a hardware-accelerated desktop decodes `hev1` while the
same build headless does not. When the browser cannot decode, the pilot now
says so on screen — it used to connect, receive every frame, and display
nothing.

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
 "maxBitrateKbps": 3000, "latencyBudgetMs": 100, "maxGopMs": 10000,
 "preferIntraRefresh": true, "suggestedFps": 0, "reason": "profile"}
{"type": "video-config", "channel": 1, "profile": "balanced",
 "maxBitrateKbps": 2250, "latencyBudgetMs": 100, "maxGopMs": 10000,
 "preferIntraRefresh": true, "suggestedFps": 0, "reason": "abr-down"}
{"type": "recovery-request", "channel": 1, "kind": "idr", "reason": "pilot-loss"}
{"type": "layer", "channel": 1, "layer": 0, "name": "low", "reason": "down"}
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

**Keyframes are on demand (ADR 0009).** `maxGopMs` is the longest the
publisher may go without a full recovery point — an IDR, or a completed
intra-refresh sweep — and it is a safety net, not the recovery mechanism:
10 s on `latency` and `balanced`, 4 s on `quality`. Seyd asks for recovery
points when a pilot needs them (`recovery-request`, below), so a publisher
should not emit an IDR every second on a timer. `preferIntraRefresh: true`
asks the publisher to refresh the picture gradually where its encoder can
(x264 `intra-refresh`, NVENC, Jetson) with any sweep period up to `maxGopMs`;
one that cannot uses the longest GOP up to `maxGopMs` and answers
`recovery-request` with an IDR (the demo camera: `GovLength` 250 at 25 fps,
`requestKeyFrame` answered in ~150 ms). The publisher-control channel is
therefore load-bearing for a pilot's *first* picture as well as for
adaptation — with nobody answering, a join waits up to `maxGopMs`.
`docs/encoder-setup.md` has the per-encoder recipes and how to verify them
with `tools/keyframe-probe.py`.

`layer` is sent when the relayed simulcast layer changes (ADR 0008), on a
channel that declared `[[channel.layer]]` entries. Seyd switches on that layer's
next keyframe regardless; a publisher that can force an IDR on the named stream
should, which is what makes a switch cost one frame rather than a GOP. `reason`
is `up` or `down`. Nothing is required of a publisher that ignores it.

`recovery-request` carries a `kind` — `ltr`, `intra_refresh` or `idr` — and is
rate-limited to one per 250 ms per channel. **Seyd states intent; the publisher
decides how to meet it.** A publisher that cannot honour `ltr` should answer
with whatever it can, and one that ignores `kind` entirely behaves exactly as
before.

The ladder starts at the cheapest rung and climbs only when a rung has had the
profile's `recovery_grace_ms` and loss continues. It resets to `ltr` after two
quiet seconds, so an isolated loss never permanently escalates a link. An IDR
is 30–60 KB against a ~2 KB delta; on a 2 Mbps uplink that spike is 120–240 ms
of serialisation, which is what field run A measured as p95.

`kind` is `idr` unconditionally when the pilot sends `request-keyframe` — its
decoder cannot start or resync without a key chunk, and that demand sits
outside the ladder rather than pinning it at the top rung. Set
`[agent] recovery_ladder = false` to force `idr` for every request.

**A publisher that answers with `ltr` or `intra_refresh` never produces a
keyframe.** Seyd stops sending delta frames after dropping one for backlog and
waits for a recovery point; without a timeout that wait never ends and the
stream latches silent. `recovery_grace_ms` in the QoS profile is that timeout —
deltas resume when it expires, and the ladder escalates if loss continues. `session` lets robot-side code park actuators when the last driver
leaves. A robot whose publisher ignores all of this keeps streaming on whatever
it was configured with.
