# The daemon, `seydd`

`seydd` is the Seyd robot agent as a daemon: the right form factor when the
camera already publishes video on a socket and the robot's own software can
talk over UDP. The user writes a TOML file and small programs on the daemon's
generic UDP interfaces. Nothing in the daemon knows any vendor; vendor code
(the demo's Hikvision ISAPI driver, `examples/demo-robot/bridge.py`) lives
outside Seyd and consumes these interfaces like any customer program.

## What it does

- **Video in** over `rtsp://` or `rtp://`: depacketizes RTP, splits access
  units into chunks. Never decodes, re-encodes or inspects the picture.
- **Sensor in** over `udp://`: each datagram becomes one message to every
  connected pilot.
- **Commands out** as UDP datagrams: each driver message on a command
  channel is written to the channel's `output`, one datagram per message,
  raw payload, nothing prepended. Commands from observers are dropped.
- **Publisher control out** as JSON datagrams on `[publisher_control] udp`:
  what Seyd asks of the encoder, and when sessions start and end.

## Build and run

```bash
cargo build -p seydd --release        # target/release/seydd; pure Rust, cross-builds are plain
seydd [--config /etc/seyd/seydd.toml] [--probe-input SECONDS] [enrol --token TOKEN [--signal-url URL]]
```

`--config` defaults to `/etc/seyd/seydd.toml`. `--probe-input N` opens the
video inputs, logs frame statistics for N seconds and exits: the fastest way
to check a camera URL before involving the cloud. `RUST_LOG` filters the log
(`info`, or `seyd_core=debug`).

## The configuration file

Every key is in `seydd-config.md` (generated from the config structs). The
shape, from `docs/protocol/seydd.md`:

```toml
[agent]
robot_id        = "my-robot"
signal_url      = "wss://seyd-signal-flj7s44j4a-ew.a.run.app/ws"
credential_path = "/var/lib/seyd/robot.key"   # Ed25519 seed, created on first run
quic_port       = 4433                         # the UDP port a forward or pinhole must open
ipv6            = true
port_mapping    = true                         # PCP / NAT-PMP / UPnP
qos_profile     = "balanced"                   # the ceiling: latency | balanced | quality
max_sessions    = 4                            # one driver, the rest observers
relay           = true                         # allow the cloud relay as the last resort (ADR 0010)
# recovery_ladder = true                       # false: every recovery request is an IDR
# enrolment_token = "seyd_enr_…"               # or SEYD_ENROLMENT_TOKEN, which wins

[[channel]]
kind  = "video"
name  = "main"
input = "rtsp://192.168.1.20:554/Streaming/Channels/101"   # or "rtp://0.0.0.0:5000"
codec = "avc1.42001f"                                       # WebCodecs string; reaches the browser unchanged
fps   = 25
# max_bitrate_kbps = 4000                                   # only if the encoder tops out below the profile's ceiling

[[channel]]
kind   = "sensor"
name   = "telemetry"
input  = "udp://127.0.0.1:5002"
codec  = "json"

[[channel]]
kind   = "command"
name   = "drive"
output = "udp://127.0.0.1:5004"
codec  = "json"

[publisher_control]
udp = "127.0.0.1:5003"
```

- Channels are numbered from 1 in file order; that numbering is what the
  pilot sees. The section is `[[channel]]`, not `[[channels]]`: the typo
  parses as an empty list and the robot announces nothing (the daemon warns).
- RTSP credentials: leave userinfo out of the URL and set `SEYD_RTSP_USER`
  and `SEYD_RTSP_PASSWORD` in the environment; they are substituted into any
  `rtsp://` URL without userinfo. URLs are redacted in every log line.
- A video channel names one stream with `input`, or several with
  `[[channel.layer]]` entries for simulcast (below); the two are exclusive.
- `max_bitrate_kbps` states the most the publisher can encode. Set it when
  the encoder's maximum is below the profile's ceiling (a drone whose radio
  tops out at 4 Mbps under `quality`'s 6 Mbps), so the controller's whole
  range scales to it and no `video-config` asks for the impossible.

## Simulcast: `[[channel.layer]]`

Where the camera already encodes the same picture twice (main and sub
stream), declare both and Seyd forwards one, switching on the target layer's
next keyframe. The robot receives every layer (fine on a LAN or a cable); the
uplink carries exactly one.

```toml
[[channel]]
kind  = "video"
name  = "main"
codec = "avc1.42001f"   # must admit the *highest* layer: 42001f is level 3.1 = 1280x720 max; 1080p needs avc1.420028
fps   = 25

  [[channel.layer]]
  name  = "low"
  input = "rtsp://cam:554/Streaming/Channels/102"

  [[channel.layer]]
  name  = "high"
  input = "rtsp://cam:554/Streaming/Channels/101"
  activate_above_kbps = 1800
```

Layers are ordered by `activate_above_kbps`, lowest first; the lowest is the
base and must be 0. All layers must be the same codec family. Same frame
rate on every rung on purpose: resolution is what degrades. Downward switches
are immediate; an upward switch needs the controller target 25 % above the
rung's activation point for 10 s. Set the camera up to match (two streams,
same fps, each meeting the publisher contract); `examples/demo-robot/
setup-simulcast.py` does it for a Hikvision.

## Enrolment

A robot must be enrolled before the cloud lets it connect
(`unknown-robot` otherwise). Three equivalent ways, all of which create
`credential_path` if missing and post the public key with the token to
`POST /api/v1/enrol` at the address derived from `signal_url`:

```bash
SEYD_ENROLMENT_TOKEN=seyd_enr_… seydd --config /etc/seyd/seydd.toml   # provisioning scripts: secret never on disk
seydd --config /etc/seyd/seydd.toml enrol --token seyd_enr_…           # interactive, separate step
# or enrolment_token = "…" under [agent]
```

A token is single-use (a second robot gets `410`). On a first run a failed
enrolment is fatal; with a credential already present it is only a warning,
so a spent token left in the environment never stops an enrolled robot. The
key file is what stays enrolled: back it up with the machine, never copy it
to a second robot (`key-mismatch`). Details in `access.md`.

## Running as a service (template; the repo ships no unit yet)

```ini
[Unit]
Description=Seyd robot daemon
After=network-online.target
Wants=network-online.target

[Service]
ExecStart=/usr/local/bin/seydd --config /etc/seyd/seydd.toml
Restart=always
RestartSec=2
Environment=RUST_LOG=info
EnvironmentFile=-/etc/seyd/seydd.env     # SEYD_RTSP_USER, SEYD_RTSP_PASSWORD, SEYD_ENROLMENT_TOKEN on first start
StateDirectory=seyd                      # /var/lib/seyd, parent of the default credential_path

[Install]
WantedBy=multi-user.target
```

## The robot-side interfaces (what the user's program does)

**Command output.** Bind the `output` port; each datagram is one driver
command with the raw payload (JSON text for `codec = "json"`). Act on it;
drop it if it is older than the stale rule allows. Prefer velocities with
a repeat while held and an explicit zero on release, and a robot-side hold
that zeroes when repeats stop.

**Sensor input.** Send one datagram per message to the `input` address; each
reaches every connected pilot as one sensor message.

**Publisher control.** Bind the `[publisher_control]` port and consume
best-effort JSON, one object per datagram, four types:

```json
{"type": "video-config", "channel": 1, "profile": "balanced",
 "maxBitrateKbps": 3000, "latencyBudgetMs": 100, "maxGopMs": 10000,
 "preferIntraRefresh": true, "suggestedFps": 0, "reason": "profile"}
{"type": "recovery-request", "channel": 1, "kind": "idr", "reason": "pilot-loss"}
{"type": "layer", "channel": 1, "layer": 0, "name": "low", "reason": "down"}
{"type": "session", "state": "started"|"ended", "session_id": "…", "role": "driver"|"observer", "sessions": 1}
```

- `video-config`: Seyd's targets for the encoder, sent once at start
  (`reason: "profile"`), on a profile change (`"pilot-request"`) and whenever
  the controller moves the request (`"abr-down"`, `"abr-up"`). Apply
  `maxBitrateKbps` as the encoder's cap, `maxGopMs` as the GOP or refresh
  bound, `suggestedFps` as a frame-rate cap when non-zero (0 restores).
  Coalesce writes to a slow camera API (the demo: one write per two seconds
  per setting, latest value wins).
- `recovery-request`: a pilot needs a recovery point now; answer with the
  cheapest `kind` the encoder has, an IDR if nothing else. This path is
  load-bearing for joins, not just adaptation.
- `layer`: the forwarded simulcast layer changed; force a keyframe on the
  named stream if possible so the switch lands on the next frame.
- `session` with `state: "ended"` and `sessions: 0`: park actuators.

The worked example is `examples/demo-robot/bridge.py`: binds both ports,
speaks the camera's ISAPI, coalesces writes, drops stale commands, homes the
camera when the last session ends. A drone under the same unchanged daemon
is `examples/tello-robot/bridge.py` (stick hold, orphan landing, altitude
limit). Full semantics in `publisher-contract.md`.

## What the inputs do when the source misbehaves

- **`rtsp://`**: when the stream ends or fails the daemon reconnects with an
  exponential backoff from 1 s to 30 s; a session that ran 30 s resets the
  backoff, so a camera that drops once costs about a second of black, and
  one that flaps does not hammer itself.
- **`rtp://`**: a plain UDP listener. It does not check SSRC; a jump in the
  sequence number is counted as loss and decoding continues from the next
  keyframe. A pipeline that restarts (new SSRC, new timestamp base) therefore
  keeps feeding the same channel; verify at rung 1 of `verification.md` that
  frames resume after a restart, because the timestamp reset is not handled
  specially.
- An RTSP source that is down at start is retried the same way, so the
  daemon may be started before the camera.

## Commands: hold, not timestamps

A vanished pilot is detected by the QUIC transport (keep-alive every 2 s,
idle timeout 10 s), so `session ended` for a pilot that simply disappeared
arrives within about ten seconds; a pilot that closes its page says goodbye
at once. Sub-second safety is therefore the robot program's job, not Seyd's:
have the pilot repeat velocities while held (the demo: every 100 to 200 ms)
and send an explicit zero on release, and have the robot zero its actuators
when the repeats stop (a hold of a few hundred ms). Act on the newest
datagram and drop the ones behind it. The demo's extra `ts` field
(`Date.now()` from the pilot's clock, compared against the camera host's
clock) is a convention of the demo page and its bridge, not a Seyd field;
use it only where both clocks are NTP-synced.

## Building for the robot

Every Seyd crate is pure Rust. On an ARM64 board (Jetson, Raspberry Pi 5)
the simplest build is on the device with rustup; a cross-build from a
workstation is `cargo build -p seydd --release --target
aarch64-unknown-linux-gnu` with a linker for that target installed (the
`cross` tool wraps that in a container). One static-ish binary and the TOML
file are the whole install.

On Windows the same `cargo build -p seydd --release` with the msvc toolchain
gives `seydd.exe`; nothing in the core is Unix-only (the gateway for
PCP/NAT-PMP comes from the IP helper API, the QUIC port is bound
`SO_EXCLUSIVEADDRUSE` so no other process can take it). Two things differ:
the defaults `--config /etc/seyd/seydd.toml` and `credential_path =
/var/lib/seyd/robot.key` are Unix paths, so pass `--config` and set
`credential_path` to a directory only the service account can read — the
key file is created with the directory's ACL, not `0600` as on Unix. Run it
as a Windows service with the tooling the integrator already uses (`sc
create`, NSSM, Task Scheduler); the repository ships no service wrapper.

## Codecs

| Codec | `codec` | Ingest | Browser |
|---|---|---|---|
| H.264 | `avc1.…` | RTSP and RTP | Everywhere |
| H.265 | `hev1.…` / `hvc1.…` | RTSP and RTP | Only with a hardware HEVC decoder on the pilot's machine (not headless Chrome) |
| Motion JPEG | `mjpeg` | RTSP only | Everywhere; about ten times the bandwidth of H.264 |

## The log, in order

`seydd starting`, one `candidate` line per discovered address, `certificate`,
`robot identity`, then per input `rtsp input playing` / `rtp input
listening` / `sensor input listening`. On a pilot: `session started` with
role and path, `session ended` with a reason. `abr` lines are controller
decisions, `simulcast` lines a layer change. `signal denied` carries
`unknown-robot` (enrol) or `key-mismatch` (another agent uses the id, or the
key file was replaced; delete the robot in the console and enrol again).
