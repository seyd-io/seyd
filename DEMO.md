# DARC Demo — Always-On Hikvision PTZ

## Purpose

A permanently-on demo robot that potential clients can discover on the fleet page, connect to, and control in real time. The goal is to let prospects experience the DARC feedback loop themselves — not watch a video of it — in a controlled environment with no moving parts to crash or break.

---

## Hardware

| Item | Model | Cost |
|---|---|---|
| PTZ camera | Hikvision DS-2DE2A404IWG1-E (4MP, 4× optical) | ~$180 |
| PoE injector or switch | Any 802.3af injector | ~$25 |
| Desk / wall mount arm | Any ¼-20 ball head arm | ~$20 |
| Agent host | Mac mini or Raspberry Pi 5 | existing / ~$80 |
| **Total** | | **~$225–305** |

The camera is always on, pointed at something visually interesting in a controlled space (a shelf, a model, a small set). Clients pan and tilt to look around.

The unit on the bench is a **DS-2DE2A404IWG1-E** (note the `1`), firmware V5.9.5
build 260129, encoder V7.3. See "Camera provisioning" below for its measured
capabilities and the settings actually applied.

---

## Architecture

```
Office LAN
┌─────────────────────┐  RTSP/TCP (H.264)   ┌──────────────────────┐
│  Hikvision PTZ      │────────────────────► │  DARC Agent          │
│  192.168.86.237:554 │◄────────────────────│  (Mac mini / Pi 5)   │
│                     │  ISAPI/XML (PTZ cmds)│                      │
└─────────────────────┘                      └──────────┬───────────┘
                                                        │ WSS
                                             ┌──────────▼───────────┐
                                             │  darc-signal          │
                                             │  (Cloud Run)          │
                                             └──────────┬───────────┘
                                                        │
                                              client browsers worldwide
```

**No FFmpeg intermediary.** PyAV (libavformat) opens the camera's RTSP stream directly. The H.264 bitstream demuxed from RTSP is identical to what PyAV currently demuxes from RTP/UDP — the downstream relay path (NAL extraction → binary WebSocket → WebCodecs) is unchanged.

Verified on the bench Mac: `ffmpeg 8.1` and PyAV 17.1.0 both expose the `rtsp`
demuxer. Note RTSP is a *demuxer* in ffmpeg's taxonomy, not a protocol — it does
not appear in `ffmpeg -protocols`, only in `-demuxers`.

---

## Camera provisioning (done — 2026-08-25)

State on the bench: activated, DHCP, on the operator LAN, encoding exactly what
the pilot's decoder expects. Credentials live in `.env.local` (gitignored, mode
600) as `CAMERA_USER` / `CAMERA_PASSWORD` — this is the answer to open question 3.

### Finding it

`tools/find-camera.py` locates the camera by Hikvision SADP and ONVIF
WS-Discovery, with an optional TCP port scan. Discovery is the part that matters:
the camera shipped on the factory static **192.168.1.64**, which is invisible to
any port scan of the operator subnet. SADP found it anyway because SADP works at
layer 2, independent of addressing — which is why the tool tries it first and why
a port scan alone is not a substitute.

The corollary: **camera and agent host must share a layer-2 segment** for
discovery to work at all. On this bench the camera was initially behind a second
router while the Mac sat behind the Google Nest's NAT, and nothing could see it.
The Nest's WAN address was in CGNAT space (`100.87.0.0/18`), which is the tell
that the upstream box was bridging rather than routing. Moving the camera's PoE
injector to the Nest's own LAN port fixed it.

#### Direct Ethernet to a bare switch (2026-09-07)

Cabling the laptop into the camera's own switch, with internet still on Wi-Fi, is
the cleanest bench setup: the camera's traffic never crosses the house NAT. There
is no DHCP server on that switch, so **both ends fall back to link-local**
(`169.254.0.0/16`, `dhcp: true` in SADP but no lease) and the camera's address
changes between power cycles. Two consequences:

- Re-run `tools/find-camera.py` after every camera reboot and update `CAMERA_IP`
  in `.env.local`. SADP finds it regardless of subnet; a port scan of the Wi-Fi
  subnet never will.
- The agent must still advertise its **routable** address. `seyd-nat` gathers on
  the default route (Wi-Fi), so the host candidate is the Wi-Fi address and the
  link-local camera address stays off the wire. Verified: srflx path, 720p25,
  g2g p50 9.7 ms.

#### Do not fail a login against this camera

Hikvision's illegal-login lock counts failed digest attempts per client IP and
then **rejects the correct password for ~30 minutes**, on every ISAPI endpoint.
An already-established RTSP session keeps streaming, so video looks healthy while
PTZ, keyframe requests and bitrate caps all return 401 — the shape of the failure
in `bridge.py`'s log is a burst of `PTZ ... → HTTP 401` with the video untouched.

The way this happens in practice is running a tool without credentials in the
environment: an empty `CAMERA_PASSWORD` is still a login attempt.
`tools/seyd-smoke.py --camera-ip` now refuses to start unless `CAMERA_PASSWORD`
is set, but the general rule stands — `set -a; . ./.env.local; set +a` before
anything that touches the camera. There is no way to clear the lock from outside;
it expires on its own, and probing it while locked may restart the timer, so wait
it out rather than polling.

The lock also exposed a second, worse failure: a 401 permanently wedges urllib's
`HTTPDigestAuthHandler`, which keeps a retry counter and afterwards raises
`digest auth failed` for every request *even once the camera is happy again*.
`hikvision.py` therefore discards its opener and retries once on any 401, so the
demo recovers by itself instead of needing a restart. Any long-lived urllib
digest client against this camera needs the same treatment.

### Activating it — do this in the browser

A factory Hikvision is **un-activated**: no password is set, and every ISAPI
endpoint except `/ISAPI/Security/userCheck` and
`/ISAPI/Security/sessionLogin/capabilities` returns `notActivated`.

`PUT /ISAPI/System/activate` cannot be driven by hand on V5.9.5. The `<password>`
field is always base64-decoded — proven by sending a 16-character plaintext and
getting `password is wrong, len = 12` back, 12 being the decoded length — and the
decoded value must be a fixed-length ciphertext. The public key needed to produce
it lives at `/ISAPI/Security/RSA/publicKey` and `/ISAPI/Security/deviceKey`, and
**both are themselves gated behind `notActivated`**. That circle does not close
from outside, on HTTP or HTTPS.

So activation is a one-time manual step through the camera's own web UI at
`http://<camera-ip>/` (it redirects to `/doc/index.html`), whose JavaScript
implements the handshake. Everything afterwards is plain **digest-auth** ISAPI and
fully scriptable. Do not spend time reimplementing the activation crypto.

Password charset note: alphanumeric-only is deliberate. It satisfies Hikvision's
"at least two of {lower, upper, digit, special}" rule via three classes, dodges
the undocumented per-firmware special-character allowlist, and embeds in an RTSP
URL with no escaping.

### Encoder settings applied

The camera is now the video publisher, so per CLAUDE.md's boundary rule these are
*its* settings — `packages/seyd-qos` still states only transport targets.
Applied to channel 101 via `PUT /ISAPI/Streaming/channels/101`:

| Setting | Value | Why |
|---|---|---|
| codec | H.264 | H.265 is offered; WebCodecs is configured for `avc1.*` |
| profile | **Baseline** | shipped as Main — see below |
| resolution | 1280×720 | the `balanced` QoS profile's target |
| frame rate | 25 fps | sensor is PAL; 2500 is the cap it offers |
| rate control | VBR, 3000 kbps cap | `balanced` bitrate ceiling |
| GOP | **from the profile**: 250 (10 s) on `balanced`/`latency`, 100 on `quality` | ADR 0009: keyframes on demand via `requestKeyFrame`; `bridge.py` writes `GovLength` from every `video-config`. Was 25 until 2026-09-08 |

Measured off the wire afterwards: `profile=Baseline level=31 1280x720
has_b_frames=0`, 25 fps, keyframes at exactly 1.000 s intervals, ~500 kbps on a
static scene against the 3000 kbps ceiling.

**GOP, re-measured 2026-09-08 (`tools/keyframe-probe.py`).** At GovLength 25
every keyframe reached the agent ~22 ms after its slot, so once a second the
frame interval read 60 ms then 18 ms — the hitch ADR 0005's presentation delay
hides — and the IDRs were 43 % of the bitrate on the static scene (696 kbps
against 394 without them). At GovLength 250 the periodic cadence is exactly
10.0 s, `requestKeyFrame` still produces an IDR 96–155 ms after the request
(35–40 ms of that is the HTTP call, then two or three frames), arrival judder at
the pilot fell from 3.7 to 0.9 ms mean and from 20 to 2 ms p95. The camera
offers no intra refresh in either codec (its `capabilities` document was read
to check), so the long GOP with on-demand IDRs is its route to ADR 0009. This
makes `bridge.py` load-bearing for a pilot's first picture: without it a join
waits up to 10 s for the periodic keyframe.

**Baseline rather than Main is load-bearing, not a preference.** Main permits
B-frames, and PROTOTYPE.md's pilot gates frames into strict monotonic decode
order — which is only correct when decode order equals display order. A stream
with B-frames would be silently reordered into corruption by that gate. Baseline
forbids B-frames outright, and `has_b_frames=0` confirms it. Baseline level 3.1
is also exactly `avc1.42001f`, the codec string the pilot already hardcodes, so
**no pilot change is needed**.

**25 fps is a publisher choice, not a DARC setting.** The QoS table in
PROTOTYPE.md says 30 fps because `sim/video-source.sh` drives a Mac webcam.
`qos.py` specifies a bitrate ceiling, a latency budget and a max GOP — never
resolution or frame rate. A PAL sensor answering the same request with 25 fps is
the boundary working as intended. Switching the camera to NTSC for 30 fps would
cost a reboot and buy nothing on a fixed demo scene.

### The degradation ladder (2026-09-07)

Under congestion the ABR lowers the bitrate ceiling, and `bridge.py` applies it
as the camera's `<vbrUpperCap>`. Below that ceiling the publisher must decide
*how* to spend fewer bits. The ordering principle is that **frame rate is a
latency term for the pilot, not a quality term**: 25 → 15 fps stretches the
interval between frames from 40 ms to 67 ms, so the operator waits up to 27 ms
longer to see the result of their own input, against a measured glass-to-glass
p50 of ~10 ms. Resolution therefore goes first and frame rate last.

Measured on this camera, which constrains how far that can be taken:

| | Channel 101 (main) | Channel 102 (sub) | Channel 103 (third) |
|---|---|---|---|
| Widths | **1280, 1920, 2560** | 352, 640, 704 | 352, 640, 704, 1280, 1920 |
| Heights | 720, 960, 1080, 1440 | 288, 480, 576 | 288, 480, 576, 720, 960, 1080 |

The demo streams channel 101 at 1280x720, which is **already that channel's
lowest resolution** — there is no resolution rung available on it at all.

A resolution change also behaves differently from the other settings. Writing
`<videoResolutionWidth>`/`<videoResolutionHeight>` returns 200 and is stored, but
the *running* RTSP session continues at the old resolution with no interruption;
only a **new** session gets the new one (verified on channel 103, 704x576 →
640x480: frame delivery continued unbroken at 9.5 fps across the change, and a
fresh `ffmpeg` connection then reported 640x480). So a resolution rung costs an
RTSP reconnect — about 83 ms on this camera, measured from `rtsp input playing`
to `first rtsp frame` — plus a decoder reconfigure on the pilot.

Frame rate, by contrast, applies to the running stream: `<maxFrameRate>` (in
hundredths, so 25 fps is 2500) takes effect with no reconnect. Verified live —
`suggestedFps: 15` moved the camera 2500 → 1500 and back with a single
`rtsp input playing` for the whole session.

**Implemented:** the frame-rate rung. `seyd-qos` sets `suggested_fps = 15` once
the controller has been pinned at its bitrate floor (25 % of the profile ceiling,
so 750 kbps on `balanced`) for 5 congested seconds; `bridge.py` applies it and
restores the camera's configured baseline when the hint returns to 0.

Note that injected pilot-side loss does **not** exercise this path: FEC recovers
it, so `residual` stays false and, on a 0.3 ms LAN with no bandwidth limit,
neither RTT inflation nor send backlog ever appears. Congestion has to be real.
The controller side is covered by `abr::tests::floor_and_suggested_fps`; the
camera side was verified by sending a synthetic `video-config` to
`udp://127.0.0.1:5003`.

**The resolution rung is simulcast, not reconfiguration** (ADR 0008). Since a
resolution change only takes effect on reconnect, and channel 101 has no lower
resolution anyway, the demo instead subscribes to *both* streams and relays one:

| layer | ISAPI channel | picture | cap |
|---|---|---|---|
| `low` | 102 (sub) | 640x480 | 800 kbps |
| `high` | 101 (main) | 1280x720 | 3000 kbps |

Both must be **Baseline** H.264 at the **same frame rate** — the sub stream ships
as Main from the factory, which would turn a switch into corruption rather than a
smaller picture. `examples/demo-robot/setup-simulcast.py` provisions both and is
idempotent; run it after a factory reset. It prefers 640x360 for a 16:9 rung and
falls back to 640x480, which this firmware's sub stream is limited to — so the
low rung is 4:3 and the picture letterboxes on switch.

Seyd switches on the new layer's next keyframe. `bridge.py` turns the `layer`
control message into an ISAPI `requestKeyFrame` on that stream, so "next
keyframe" becomes immediate: the switch costs one frame, against the ~83 ms a
reconnect would have cost.

Measured end to end on this camera at 12 % injected loss: one drop to `low`, no
flapping across 150 s, and a climb back to `high` once the target recovered past
what it had learned. The pilot's canvas follows (1280x720 → 640x480 → back) with
no decoder errors — WebCodecs takes the resolution change in its stride because
inputs are Annex B with parameter sets inline on every keyframe.

### Watch items

- **`pix_fmt` is `yuvj420p`** — full-range YUV, not the usual limited range. If
  the canvas render comes out crushed or washed out, colour range is the first
  thing to check, not the encoder.
- **The DHCP lease is not reserved.** The camera is at `192.168.86.237` today by
  lease, not by contract. For an always-on demo either add a DHCP reservation on
  the router or have the agent resolve the camera by SADP at startup — keyed on
  MAC `8c:22:d2:5d:58:e7`, which is stable.
- **Network changes need a reboot.** `PUT .../Network/interfaces/1/ipAddress`
  returns `rebootRequired` and does nothing until `PUT /ISAPI/System/reboot`,
  which needs a genuinely empty body — an XML declaration alone is rejected as
  `badXmlFormat`.

---

## Running it on the Seyd stack (current)

```bash
./demo-seyd.sh                                   # against the deployed cloud (default URL in the script)
SIGNAL_URL=ws://localhost:8080/ws ./demo-seyd.sh # against a local signal server (seyd-cloud)
CAMERA_IP=192.168.86.237 DARC_QOS_PROFILE=latency ./demo-seyd.sh
```

The deployed pilot page is
`https://seyd-signal-flj7s44j4a-ew.a.run.app/?robot=seyd-demo`. **Open it in
Chrome or Edge** — the web pilot is Chromium-only, and iPhones and iPads cannot
run it at all because every iOS browser is WebKit (see SPEC.md, "Browser
support"). Demoing from a phone means an Android handset until the native SDKs
land. On the robot's own LAN, Chrome will ask for "local network access" before
it can use the direct `host` candidate; denying it still works via the router
hairpin (`srflx`), just with ~12 ms more latency.

`demo-seyd.sh` preflights the camera over ISAPI (one clear line instead of a
daemon retrying forever), derives a `seydd.toml` from
`examples/demo-robot/seydd.toml`, and starts `seydd` plus the bridge.

`seydd` (generic daemon, `examples/demo-robot/seydd.toml`) pulls the camera's
RTSP stream and forwards `ptz` command messages to `udp://127.0.0.1:5004` and
publisher-control messages (`recovery-request`, `session`) to
`udp://127.0.0.1:5003`; `examples/demo-robot/bridge.py` consumes both and
speaks ISAPI (`examples/demo-robot/hikvision.py`, moved out of the agent).
Keyframe-on-join is therefore automatic: the pilot's `request-keyframe` on
`hello` becomes a `recovery-request` which the bridge turns into
`PUT /ISAPI/Streaming/channels/101/requestKeyFrame`. The pilot page is
`web/demo` (`?robot=seyd-demo`). Verify with
`tools/seyd-smoke.py --robot seyd-demo`.

## Legacy notes (the Python prototype, deleted 2026-08-28)

The sections below describe the prototype that first ran this demo; the
mechanisms (RTSP over TCP, momentary PTZ windows, latest-value-wins, park on
release) carried over into `seydd` and `examples/demo-robot/`.

## Agent changes (implemented)

### 1. Dual video input mode

`--video-url` is an alternative to `--video-port`. When set, `peer.py` opens the RTSP URL directly via `av.open()` instead of reading from an SDP file:

```python
# current (UDP RTP via SDP file)
container = av.open(sdp_path, format='sdp', options={...})

# new (RTSP direct)
container = av.open('rtsp://admin:password@192.168.86.237:554/Streaming/Channels/101',
                    options={'rtsp_transport': 'tcp', 'fflags': 'nobuffer', ...})
```

Everything downstream of `container.demux()` is identical. No other changes to the relay path.

Two details that are not obvious: RTSP is pulled over **TCP**, because an IP
camera's RTP/UDP has no FEC and its losses would arrive as corrupt access units
that DARC then spends parity protecting — the camera hop is a short LAN link
where a retransmit is free, and the path worth protecting is the one after the
agent. And the video stream is selected explicitly, because a camera carrying
audio would otherwise have AAC packets chunked as access units and fed to a
`VideoDecoder` as delta frames.

Credentials are injected into the URL from `CAMERA_USER` / `CAMERA_PASSWORD` at
startup, so nothing on the command line carries them. Anything that might log a
URL goes through a redactor first — libavformat puts the full URL into its
exception messages, so the password leaks on the *error* path even when the happy
path is careful.

### 2. PTZ command handler

The agent needs a camera control adapter that:
1. Receives `{"type": "ptz", "pan": <-100…100>, "tilt": <-100…100>}` on the data channel
2. Calls the Hikvision ISAPI PTZ endpoint:

```python
import os
import requests
from requests.auth import HTTPDigestAuth

CAMERA_IP = os.getenv('CAMERA_IP', '192.168.86.237')
CAMERA_AUTH = HTTPDigestAuth(os.getenv('CAMERA_USER', 'admin'),
                             os.getenv('CAMERA_PASSWORD', ''))

PTZ_XML = ('<?xml version="1.0" encoding="UTF-8"?>'
           '<PTZData><pan>{pan}</pan><tilt>{tilt}</tilt></PTZData>')


def ptz_move(pan: int, tilt: int):
    requests.put(
        f'http://{CAMERA_IP}/ISAPI/PTZCtrl/channels/1/continuous',
        data=PTZ_XML.format(pan=pan, tilt=tilt),
        headers={'Content-Type': 'application/xml'},
        auth=CAMERA_AUTH,
        timeout=0.5,
    )


def ptz_stop():
    ptz_move(0, 0)
```

Implemented in `examples/demo-robot/hikvision.py` (formerly `packages/agent/camera.py`); `peer.py`'s `handle_message()`
dispatches `type == 'ptz'` and `type == 'ptz-home'` to it. The snippet above is
the shape of the request, not the shipped code — see that file for the real one,
which uses stdlib `urllib` rather than `requests` to keep the agent on
pure-Python wheels.

**ISAPI speaks XML, not JSON.** An earlier version of this document passed
`json={'PTZData': {...}}`; the device rejects that. Auth is **digest**, not basic
— `requests`' `auth=(user, pass)` tuple sends basic and fails.

**`momentary`, not `continuous`.** `continuous` is a velocity command with no
expiry: the camera moves until explicitly stopped. Over the public internet, with
an operator whose laptop can sleep and whose tab can close mid-gesture, "stop" is
a message that sometimes does not arrive — and the failure mode is a camera that
pans into its mechanical stop. `momentary` carries a duration and expires on its
own, so a lost stop costs one window instead. Verified: `pan=40` for 500 ms moved
1.89° and halted with no further command.

The agent sends 600 ms windows and the pilot renews every 200 ms while held, so
motion is smooth but an abandoned gesture dies in well under a second. An
explicit stop still goes out as `continuous` zero, which halts immediately rather
than letting the last window run out.

**Latest-value-wins.** A held key generates commands faster than an HTTP round
trip completes, so `camera.py` keeps one request in flight and one pending
target, discarding superseded ones unsent — the same single-slot design the video
frame sender uses, for the same reason. Measured: 20 rapid conflicting inputs
collapsed to 6 requests.

**Park on release.** Any way of losing the operator is a way of losing the stop
message, so the agent stops and returns home on pilot disconnect, and the pilot
also stops on window blur and tab hide. Verified by killing a client mid-pan: the
camera travelled a further 3.7° and was back at home within 2 s.

Measured: `pan=30` moves ~21°/s (azimuth 1800 → 1971 in 0.8 s). Azimuth and
elevation are in hundredths of a degree via
`GET /ISAPI/PTZCtrl/channels/1/status`; `PUT .../absolute` with an
`<AbsoluteHigh>` block returns to an exact position, which is what `--ptz-home`
uses.

### 4. Commands in relay mode

PTZ is the entire interaction here, and a client behind carrier NAT lands on the
relay path — where, before this work, there was no pilot→robot JSON channel at
all. Relay mode now tunnels JSON both ways through the signal server; see
PROTOTYPE.md's `cmd` / `cmd-out` messages. Sensor data and telemetry, which had
also been silently dead on that path despite the docs claiming otherwise, work
there now too.

### 3. New CLI flags

```
--video-url        rtsp://... (alternative to --video-port for IP cameras)
--video-fps        source frame rate; scales the backlog drop threshold (25 here)
--camera-ip        camera address for ISAPI PTZ control
--camera-channel   PTZ channel (default 1)
--ptz-home         park position as elevation,azimuth,zoom — e.g. 0,1800,10
```

No `--camera-pass`. Credentials come from `CAMERA_USER` / `CAMERA_PASSWORD` in
the environment: a password in an argument is readable by any local process via
`ps`, which defeats the point of keeping it out of the repo.

---

## Pilot changes (implemented)

### PTZ control

**Drag-on-canvas plus arrow keys**, of the three options originally listed.
Overlay arrow buttons were rejected because they cover the picture the operator
is trying to look at and give four directions where the camera has infinitely
many. The canvas drag is a virtual joystick measured from the centre of the
frame, with a 12% deadzone and speed ramped from zero at the deadzone edge — a
linear map straight from the deadzone jumps to 12% speed exactly where an
operator makes their finest corrections.

Pointer events rather than mouse events, so touch works: a prospect on a phone is
a likely visitor. `touch-action: none` on the canvas is load-bearing there —
without it the browser claims the drag for scrolling and the camera never moves.
The pointer is captured on press so a drag leaving the canvas still delivers its
release; an uncaptured pointer released outside the element leaves the camera
moving.

Arrow keys rather than WASD: `S` is already the stats toggle, and silently
stealing it — or moving stats elsewhere — is worse than using the keys that need
no explanation on a camera. Shift is a speed modifier and is re-evaluated on
release without ending the gesture.

Gamepad support was not built. It would feel best of the three and is the obvious
next addition if the demo is shown at a stand rather than over a link, but it
serves the fewest visitors per line of code.

The stop command goes out on pointer release, key release, **window blur, and tab
hide**. The last two matter more than they look: a closed laptop lid or a switched
tab is the common way an operator stops steering without telling anyone.

---

## Demo environment

- Camera pointed at a curated, visually interesting scene (model, art, branded backdrop, window view)
- Pan range is physically limited by mount position — no need to guard against hitting stops
- Robot ID: `darc-demo` (always listed on fleet page)
- Latency notice shown on pilot page: "You are controlling a physical camera. Response latency reflects your connection."

---

## Open questions

1. ~~**Multiple simultaneous pilots**~~ — **solved on the Seyd stack.** The
   cloud assigns one **driver** and any number of observers up to the robot's
   `max_sessions`; the agent drops commands from observers, and the pilot page
   shows a role badge. A failed session releases the driver slot immediately.
2. ~~**PTZ presets**~~ — **settled.** The camera returns to `--ptz-home`
   (default `0,1800,10`) on pilot disconnect and on `H`. Implemented with
   `PUT .../absolute` rather than stored presets, so the position lives in the
   robot's config instead of in camera state a future reset would erase.
3. ~~**Credentials management**~~ — **settled.** `.env.local` at the repo root,
   gitignored and mode 600, holds `CAMERA_USER` and `CAMERA_PASSWORD`. The agent
   reads them from the environment; they never appear in CLI args, where they
   would be visible to any local `ps`.
4. ~~**Gamepad vs mouse control**~~ — **settled.** Canvas drag is primary, arrow
   keys secondary, gamepad not built. See "Pilot changes" for why.
5. **Demo branding**: should the pilot page show different copy ("You are controlling a real camera") vs the generic UI?

6. ~~**Join latency**~~ — **solved on the Seyd stack.** The pilot sends
   `request-keyframe` on `hello` (and on any unrecoverable loss); `seydd` turns
   it into a `recovery-request` on the publisher-control port and
   `examples/demo-robot/bridge.py` calls the camera's `requestKeyFrame` ISAPI
   endpoint. Measured join-to-first-frame is now well under 300 ms.

---

## Verified end to end (2026-08-25)

Both transport paths of the prototype, against the real camera (its harnesses
are gone with it; today's check is `tools/seyd-smoke.py`):

| | P2P (WebTransport) | Relay (via signal server) |
|---|---|---|
| winning candidate | `host` | — |
| video chunks | 1239 | 2097 |
| frames decoded / assembled | 330 decoded, canvas 1280×720 | 250/250 complete |
| bad chunk headers | 0 | 0 |
| `capabilities` received | yes (via `hello`) | yes |
| telemetry | yes | yes (`agent-stats` ×10) |
| PTZ moves the camera | yes — 79.7° on synthetic ArrowRight | yes — 57.6° |
| clean stop, holds position | yes | yes |
| park on client vanishing | yes — home within 2 s | yes |

Two bugs were found only because these tests exist, both on the primary P2P path
and both silent: `capabilities` being dropped before the JSON stream id was
known, and `hardwareAcceleration: 'prefer-hardware'` failing `configure()`
outright where no hardware decoder exists. See PROTOTYPE.md for both.
