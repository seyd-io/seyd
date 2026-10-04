# Seyd Demo — Tello Drone

## Purpose

A drone a prospect can fly from a browser: the Seyd feedback loop with
something that actually moves in three dimensions, on a €100 aircraft light
enough to fly indoors. It is the second demo robot after the PTZ camera
(DEMO.md) and the first with a vehicle in the loop, so it is also where the
safety rules a customer's own bridge needs were written down.

Status (2026-10-03): flown, in the room and off the LAN (drone behind 4G,
pilot behind 5G, direct path). Forward-on-loss works; at the drone's range
limit the keyframes stop surviving its radio, see "Range flight". "Bench
results" has what was measured.

---

## Hardware

| Item | Model | Cost |
|---|---|---|
| Drone | Ryze Tello (DJI flight stack, 720p camera, ~13 min flight) | ~€100 |
| Agent host | This laptop: Wi-Fi to the drone, Ethernet to the internet | existing |
| Spare batteries | 2× | ~€40 |

The Tello is its own Wi-Fi access point (`TELLO-xxxxxx`, 192.168.10.1, no
internet). The laptop joins it on Wi-Fi and keeps the cloud on Ethernet;
`demo-tello.sh` refuses to start unless the default route leaves over
something other than the drone's interface, because otherwise the robot
would be "online" nowhere.

---

## Architecture

```
Drone Wi-Fi (192.168.10.0/24)                 laptop                              internet (Ethernet)
┌──────────────┐  UDP 8889 binary protocol  ┌────────────────────────────┐
│  Tello       │◄──────────────────────────►│ examples/tello-robot/      │
│ 192.168.10.1 │  sticks 50 Hz, take-off,   │   bridge.py + tello.py     │
│              │  land, start-video, rate   │        │  ▲                 │
│              │──────────────────────────► │   RTP  │  │ JSON            │
│              │  UDP 6038: H.264 Annex B,  │ :5010  │  │ :5014 flight    │
└──────────────┘  2-byte header per dgram   │ :5012  ▼  │ :5013 control   │      ┌──────────────┐
                                            │        seydd ──────────────┼─WSS─►│ seyd-signal  │
                                            │        QUIC/WebTransport ◄─┼──────┼─ pilot page  │
                                            └────────────────────────────┘      └──────────────┘
```

`seydd` is unchanged and knows nothing about drones: `examples/tello-robot/seydd.toml`
declares a video channel fed by RTP, a `telemetry` sensor channel and a
`flight` command channel, on ports distinct from the camera demo's so both
robots can run on one host. Everything Tello-specific is in `examples/`.

**The bridge (`bridge.py`)** is what a customer would write for their own
vehicle, and it does four things:

- **Video.** The drone streams raw H.264, not RTP. `tello.py` reassembles
  each picture from the 2-byte-headed datagrams and `h264rtp.py` re-frames it
  as RFC 6184 RTP (single NAL and FU-A, 90 kHz timestamps from arrival time,
  marker on the picture's last packet) into `seydd`'s `rtp://` input. No
  FFmpeg in the path.
- **Commands.** `flight` messages are stick velocities in −100..100 plus
  `takeoff` and `land`; the pilot repeats them every 100 ms while a key is
  held and the bridge holds each for 400 ms, then centres the sticks. The
  drone itself is fed stick packets at 50 Hz regardless, which is what keeps
  its controller link alive.
- **Telemetry.** Battery, height, speed, heading, position (visual
  odometry), flight time, Wi-Fi strength and the bridge's own video counters,
  as JSON at 10 Hz. The pilot's footer formats it.
- **Publisher control.** `recovery-request` → the drone's "start video"
  command, which is how it is asked for a keyframe; `video-config` →
  the encoder level (1–5 = 1–4 Mbps) nearest below `maxBitrateKbps`, and
  `maxGopMs` as the keyframe safety net (ADR 0009); the last session ending →
  land.

### Loss on the drone's Wi-Fi

The Tello's 2.4 GHz link loses datagrams, and every picture is one slice
across about nine of them, so one lost datagram is one lost picture. A torn
picture is never forwarded (Seyd's whole-picture rule, one hop early) and a
keyframe is requested at once, repeated every 500 ms until it arrives —
about 50 ms on the real drone. A lost *last* datagram leaves no gap in the
packet indices; the missing end-of-frame flag is the only evidence, and the
bridge treats it as a loss once it has seen the drone use the flag.

What happens to the delta frames in between is `--after-loss`:

- **`forward` (default).** They keep flowing and the decoder conceals the
  missing reference: a brief smear, motion uninterrupted. This is what the
  vendor app does.
- **`wait`.** They are held back until the keyframe: a brief freeze.

`wait` was the first design and the first flights showed why it is wrong for
a pilot (2026-10-02): the link tore 5–15 pictures per 30 s in the quiet
stretches and 60–75 in bursts, at any bitrate and with nobody connected, on
channel 8 among neighbours on 2, 4, 6, 8, 10 and 11 — and each tear was a
visible hitch. Measured on the simulator at 3 % datagram loss, 1 s GOP, no
keyframe on request (the worst case), headless Chrome: `wait` showed
5 frames/s, `forward` 21, with zero decoder errors either way. Hardware
decoders are not yet tested with `forward`; `--after-loss wait` is the
fallback if one rejects frames with a missing reference.

The channel declares `max_bitrate_kbps = 4000`, the drone's top encoder
level, so the `quality` profile's 6 Mbps ceiling becomes 4 and the rate
controller no longer steers a range the encoder cannot follow.

---

## The Tello protocol, as used

Learned from TelloPy (https://github.com/hanyazou/TelloPy) and the
TelloPilots wiki; `tello.py`'s module comment has the details. In short:

- **Binary protocol on UDP 8889**, not the text "SDK mode": it carries
  continuous stick input, pushes telemetry unprompted and keyframes the video
  on request. Framing: `0xcc`, 13-bit length `<< 3`, CRC-8, packet type,
  command id, sequence, payload, CRC-16 (both CRCs table-driven with the
  protocol's seeds). `test_tello.py` checks every packet type the bridge
  sends against bytes produced by TelloPy.
- **Handshake** `conn_req:` + the video port as a little-endian 16-bit
  value; `conn_ack:` back. Control is answered to local port 9000, video
  arrives on 6038.
- **Sticks** (0x50): roll, pitch, throttle, yaw as 11-bit values around
  1024 ± 660, packed with a fast-mode bit into six bytes, plus the time.
  The bridge never sets fast mode.
- **Telemetry**: 0x56 flight data (height, speeds, battery, state bits),
  0x1a Wi-Fi strength, and 0x1051 log records (visual-odometry position and
  velocity, IMU quaternion) once the 0x1050 header is acknowledged.
- **Video**: each datagram = frame number, packet index with bit 7 on the
  last packet, then the next slice of an Annex B elementary stream at
  960×720 (4:3). "Start video" (0x25) makes the drone send SPS/PPS; the
  bridge caches them and puts them in front of every IDR, because a joining
  decoder cannot start from an IDR without its parameter sets. The stream's
  profile is logged from the SPS on first video (`avc1.PPCCLL`); the
  channel's `codec` in `seydd.toml` should match it.

---

## Safety

Written for a vehicle that flies indoors near people, in the order things
fail:

1. **Sticks expire.** A stick command is held for 400 ms and renewed every
   100 ms while the pilot holds a key. A closed tab, a dropped link or a
   sleeping laptop leaves the drone hovering, not flying on.
2. **Stale commands are ignored** (`--max-command-age-ms`, 1.5 s), as in the
   camera demo: a command that sat in a queue is not the operator's intent.
3. **Nobody flying? Land.** When the last session ends while airborne, or
   five seconds pass with a driver present but silent, the bridge lands the
   drone (`--orphan-land-s`).
4. **The drone's own failsafe** lands it when the controller link is lost;
   the bridge exiting lands it too.
5. **Altitude limit** (`--alt-limit-m`, 5 m) is written to the drone before
   every take-off. `--stick-scale` tames the sticks for a small room;
   `--no-takeoff` refuses take-off for bench work with the propellers off.
6. **Battery**: take-off is refused below 15 %; the footer shows `LOW` when
   the drone reports it.
7. **L, or the land button, is the stop.** It centres the sticks first, works
   while a key is held, and is the one control that also works for the
   attendant at the laptop (Ctrl-C the demo: the bridge lands on exit).

A public `drive` grant (as on the other demo robots) means anyone with the
link can fly it. The drone is only powered while an attendant runs the demo;
keep it that way, or replace the public grant with console invitations.

---

## Pilot page

The pilot (`/pilot/`) enables a control scheme per declared command channel:
`ptz` gets the camera controls, `flight` the drone's
(`web/demo/src/flight.ts`). Keys: arrows move (pitch/roll), R/F climb and
descend, Q/E turn, Shift is fast, T takes off, L lands; a drag on the picture
is a pitch/roll joystick for touch, and the header gains *take off* and
*land* buttons. W/A/S/D are unused on purpose: S is the HUD everywhere on the
page. A USB gamepad works too (verified with an iBuffalo Classic USB, which
Chrome exposes in the standard layout): d-pad is pitch/roll, L/R turn, X
climbs, B descends, A is fast, Start takes off, Select lands — press any
button first, Chrome hides a pad until then. The footer line reads e.g.
`telemetry: BAT 87% · ALT 1.2 m · SPD 0.3 m/s · HDG 90° · T+0:42 · WIFI 88 · 30 fps 1500 kbps`.

---

## Running it

```
./demo-tello.sh                 # laptop on the drone's Wi-Fi, internet on Ethernet
./demo-tello.sh --fake          # no drone: fake_tello.py plays one on localhost
BRIDGE_ARGS=--no-takeoff ./demo-tello.sh     # bench, props off
```

Pilot URL: `https://seyd-signal-flj7s44j4a-ew.a.run.app/pilot/?robot=seyd-tello`.
The robot `seyd-tello` is enrolled in org "Seyd" with a public observe+drive
grant (2026-09-22); its key is `examples/tello-robot/.robot.key`. First
start on a new key: `ENROL_TOKEN=seyd_enr_… ./demo-tello.sh`.

Verification without hardware, all on one machine (what was run on 2026-09-22):

```
python3 -m unittest discover -s examples/tello-robot -p 'test_*.py'   # 21 tests
(cd cloud/api && PORT=8080 SEYD_DEV_OPEN_ENROLMENT=1 SEYD_DEV_ALLOW_ANONYMOUS=1 \
   SEYD_STATIC_DIR=$PWD/../../web/demo/dist node dist/index.js &)
python3 examples/tello-robot/fake_tello.py [--loss 0.03 --gop 30] &
python3 examples/tello-robot/bridge.py --drone-ip 127.0.0.1 &
./target/release/seydd --config <seydd.toml with ws://localhost:8080/ws and the flight output on :5024> &
tools/.venv/bin/python3 tools/seyd-smoke.py --robot seyd-tello --command flight --command-port 5024
```

Result without loss: 960×720 at 30 fps decoded in headless Chrome, glass-to-
glass p50 0.6 ms on loopback, telemetry formatted, ArrowUp produced `pitch:
35` repeats and one explicit zero on the flight channel. Take-off, stick
input, hold expiry and landing were driven through the bridge's UDP port and
confirmed in the fake drone's log.

---

## Bench results (2026-10-02, drone on the desk, motors off)

Measured with the driver itself against the real drone, laptop on the drone's
Wi-Fi (192.168.10.2), default route on Ethernet.

| Question | Answer |
|---|---|
| Connects, telemetry | `conn_ack` at once; flight data at 9.9 Hz; no CRC failures in ~1 000 packets |
| Stream | H.264 **Main profile, level 4.0** (`avc1.4d4028`), 960×720, 28–30 fps — now the `codec` in `seydd.toml` |
| Keyframe on request | **Yes**: an IDR arrives ~50 ms after "start video". **Never on its own**: no IDR in 8 s without a request. The drone is exactly the ADR 0009 publisher — on demand, with the bridge's `maxGopMs` safety net as the only timer |
| Parameter sets | SPS and PPS each arrive as a picture of their own, and the first IDR can arrive *before* them. The bridge now refuses to forward an IDR it has no parameter sets for and asks again |
| End-of-frame flag | Used (bit 7 of the index byte), so a lost last datagram is detected |
| Encoder levels | 1 = 1.0, 2 = 1.5, 3 = 2.0, 4 = 3.0, 5 = 4.0 Mbps, each within 1 %; 0 (auto) sat at 4 Mbps. The wiki's table was wrong for 4 and 5 |
| Loss on the drone's Wi-Fi | 0 pictures torn in 5 s windows at 1–2 Mbps; 1–2 per 5 s at 3–4 Mbps (Wi-Fi strength 90). The `latency` profile's 1.5 Mbps is the clean operating point |
| Browser | Smoke test passes through bridge → seydd → pilot page: 960×720 decoding at 30 fps in headless Chrome |

### First flight (2026-10-02)

Took off, hovered and landed from the deployed pilot page, direct path over
the drone's Wi-Fi interface. Three things came out of it:

- **A hands-off hover was landed after 5 s.** The orphan rule read "no
  command" as "no pilot". The pilot page now sends its sticks once a second
  even when centred, as presence, while the tab is visible; the rule itself
  is unchanged.
- **Take-off refused with no explanation.** The battery was at 13 % (the
  drone blinks red; the bridge's floor is 15 %). Refusals and automatic
  landings now travel in the telemetry as `notice` and show first on the
  pilot's footer for 8 s.
- **Camera latency felt high.** Seyd's share, measured on the pilot
  (agent to display, 12 s each, relayed through the cloud because headless
  Chrome may not use the LAN path): p50 30 ms / p95 34 ms on `latency`
  (50 ms presentation delay), p50 35 ms / p95 40 ms on `balanced` (100 ms).
  The rest is inside the drone — capture, encode and its Wi-Fi — and is not
  yet measured; it needs a clock-in-picture test (docs/latency-sources.md).
  Idle on the desk with video on, the battery fell from 22 % to 10 % in
  about ten minutes.

### Second flight (2026-10-02): forward-on-loss confirmed

Same room, `latency` profile, about 100 s in the air. 36 pictures torn on
the drone link, none held back, 40 frames shown with a missing reference:
the keyframe repaired each tear about one frame later and the hitching was
gone. The flight ended on a flat battery (the drone lands itself and then
powers off; take-off had been refused at 8 %).

### Range flight (2026-10-03): drone behind a 4G router, pilot behind 5G

The first flight off the LAN. The drone laptop sat behind a 4G router, the
pilot behind a 5G one; the session went **direct** through the 4G router's
PCP port mapping, round trip 61–74 ms, on `balanced`. The drone flew from
next to the laptop out to about 100 m and back. Up close it was fine; at
distance the picture smeared and did not recover.

What the drone link did, per 30 s (bridge counters):

| | Close (11:35) | 11:37 | 11:39:37 | Far (11:40:07) | Back (11:41) |
|---|---|---|---|---|---|
| Pictures torn | 7 | 90 | 61 | 61 | 30 |
| Keyframes requested | 10 | 40 | 51 | 80 | 16 |
| Keyframes received | 10 | 26 | 12 | **1** | 13 |
| Bytes from the drone | 1.2 Mbps | 1.8 Mbps | 0.6 Mbps | 0.3 Mbps | 1.5 Mbps |

The reading:

- **At range the drone's own radio is the bottleneck.** Ryze quotes 100 m
  as the Tello's limit. Its throughput fell to a tenth and two pictures per
  second arrived torn.
- **Keyframes were requested; they did not survive.** A keyframe is about
  14 datagrams against 9 for a delta, so at that loss rate the small frames
  still get through while the keyframe is torn almost every time. The bridge
  was already asking twice a second (80 requests, 1 answer in the worst
  window). That is why the smear never repaired, and why a pilot-side
  "request keyframe" key — five lines, `SeydSession.requestKeyframe()`
  exists — would not have helped here: it sends the request the bridge was
  already sending. Decided not to add it for this reason; it is not wrong,
  just not the lever.
- **The pilot leg was also lossy**, a few percent on 5G↔4G: the rate
  controller alternated `residual`/`recover` between 0.9 and 3 Mbps the
  whole flight. That leg is Seyd's and its FEC and adaptation were doing
  their job; `balanced` also pushed the drone to 2–3 Mbps whenever the pilot
  leg allowed it, which is the worst setting for a weak drone link.

What would help at range, in order:

1. **Adapt the encoder to the drone link, in the bridge.** The bridge sees
   torn pictures per second directly; Seyd cannot, because loss is measured
   on the pilot leg only (ADR 0006). Dropping to encoder level 1 when tears
   climb makes every frame — above all every keyframe — a third of the size,
   so more survive. Robot-side code; not built yet.
2. **Fly on `latency`**, which keeps the drone at 1.5 Mbps regardless of
   what the pilot leg could carry.
3. **Accept the hardware.** Beyond the Tello's range nothing in software
   makes the radio reach further.

Two observations on the safety rules, not changed:

- At 11:42:55, far out, the 5G leg stalled for 5 s, the presence heartbeats
  stopped and the orphan rule landed the drone where it was. Whether a link
  stall at range should land at once or hover for 15–20 s first is the
  owner's call; the drone hovers on its own with centred sticks either way.
- At 11:30:16 the same rule fired 3 s after a reconnect, because the
  last-command timestamp carried over from the previous session. The drone
  was already landing, so nothing happened; the timestamp should reset when
  a session starts.

Still open: true glass-to-glass latency, telemetry units (needs a hover and
a tape measure), heading from the quaternion (needs a turn), the stick path
into the real drone (the smoke test deliberately kept stick input away from
it), and how the signal WebSocket behaves over a long session with the
drone's Wi-Fi joined.

Then fly: hover in place with T and L only, then arrows at default speed,
before anyone else gets the link.
