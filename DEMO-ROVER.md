# Seyd Demo — Remotely Driven Rover (planned)

## Purpose

A second demo, complementing the always-on PTZ camera (DEMO.md): a prospect
books a session and **drives a small rover they cannot see in person**, over
the public internet, on the Seyd stack. It is both an architectural proof of
concept — the agent rides the robot, everything leaves over one radio, P2P to
the operator — and the demo where sub-100 ms glass-to-glass stops being a
number on a slide and becomes something the driver feels in their hands.

Unlike the camera demo this is **not public and not unattended**. Sessions are
booked; an attendant is physically present at the rover site to reposition it,
supervise, and stop it. Nothing here is implemented yet; this document is the
plan.

Status: **planned** — hardware not yet ordered.

---

## Design decisions already made

- **The base is deliberately brainless.** A chassis with a motor-driver MCU
  and nothing else — no onboard sensors, no protective senses, no computer.
  Seyd's agent and the cameras provide everything above the wheels. Safety
  therefore comes from the demo environment and the stop layers below, not
  from the vehicle.
- **The agent rides the rover.** A spare laptop (own battery — a Mac mini was
  rejected because it needs AC) sits on the chassis running `seydd` and the
  bridges. Cameras plug into it by Ethernet; only QUIC/P2P traffic crosses
  Wi-Fi. This is the customer topology, which is the point of the demo.
- **Attended, booked sessions only.** The public fleet page does not list this
  robot. The existing driver/observer session model fits as-is: the booked
  prospect takes the driver slot; the attendant can join as an observer and
  watch exactly what the driver sees.

## Hardware

| Item | Model | Cost |
|---|---|---|
| Chassis | Waveshare UGV02 — 6-wheel 4WD, all-metal, ESP32 driver board, 4 kg payload | ~$180 |
| Batteries | 3× 18650 (chassis UPS module, charge-while-running) | ~$20 |
| PTZ camera | Hikvision DS-2DE2A404IWG1-E (the DEMO.md bench unit; 560 g, runs on 12 VDC directly — PoE not needed) | existing |
| Camera power | 12 V buck/boost regulator from the chassis UPS rail | ~$15 |
| Physical e-stop | Mushroom button inline with the drive battery | ~$10 |
| Agent host | Spare laptop with working battery, lid closed, sleep disabled | existing |
| **v2 additions** | | |
| Front camera | Fixed wide-angle Hikvision turret, H.264, 12 V (same RTSP/ISAPI class, no new code) | ~$40–70 |
| Ethernet switch | Small unmanaged 5-port, 5 V | ~$15 |
| **Total new spend** | | **~$225 (v1) / ~$300 (v2)** |

Why the UGV02: 4 kg payload against a 560 g camera plus laptop; a documented
JSON command protocol (`{"T":1,"L":0.5,"R":0.5}` wheel speeds) over serial,
USB, HTTP and ESP-NOW; open Arduino firmware
(`waveshareteam/ugv_base_general`); and — verified in that firmware's source —
a **heartbeat failsafe**: `heartBeatCtrl()` zeroes the goal speed when no
command arrives within a configurable delay (`{"T":136,"cmd":<ms>}`). That is
the drive-channel equivalent of the momentary PTZ windows in DEMO.md, already
in the vehicle.

## Architecture

```
                    ┌─────────────────── rover ───────────────────┐
                    │  ┌──────────┐ Ethernet ┌─────────────────┐  │
                    │  │ PTZ cam  ├──────────┤ laptop           │  │
                    │  │ (12V)    │◄─ISAPI───┤  seydd           │  │
                    │  └──────────┘          │  ptz bridge      │  │
                    │  ┌──────────┐ USB serial  drive bridge    │  │
                    │  │ ESP32    ├──────────┤                  │  │
                    │  │ driver   │          └────────┬─────────┘  │
                    │  └────┬─────┘                   │ Wi-Fi      │
                    │   [e-stop]──[battery]           │            │
                    └─────────────────────────────────┼────────────┘
    attendant phone ── Wi-Fi ── {"T":1,"L":0,"R":0} ──┤ (direct to ESP32 HTTP)
                                                      │
                                           signal cloud / P2P pilot
```

The laptop drives the ESP32 over **USB serial** (it is physically mounted on
the rover; no reason to spend a Wi-Fi hop on it). The attendant's stop goes
over the LAN **directly to the ESP32's HTTP endpoint**, bypassing laptop,
agent and pilot entirely.

`seydd` needs no changes for v1: the drive channel is one more
`[[channel]] kind = "command"` to a UDP port, consumed by a drive bridge that
speaks serial JSON — the same pattern as `examples/demo-robot/bridge.py`
speaking ISAPI.

## Safety — three layers, in order of authority

1. **Heartbeat (automatic, in firmware).** Motors stop when commands stop —
   covers crashed laptop, dropped Wi-Fi, vanished pilot. Set the delay to
   ~500 ms; the pilot renews drive commands while held, exactly like the PTZ
   momentary-window design.
2. **Attendant soft-stop (network).** A bookmark on the attendant's phone
   hitting the ESP32 directly. **A local stop must latch:** the drive bridge
   treats it as "ignore all pilot drive commands until locally re-armed,"
   otherwise the driver's next held key un-stops the rover one heartbeat
   later. That latch is the difference between a stop and a pause.
   (Later hardening: a spare ESP32 as an ESP-NOW kill-fob — the firmware has
   ESP-NOW group control — survives even the Wi-Fi AP dying.)
3. **Physical e-stop.** No network message actually cuts power, and
   "hazardous" is exactly when software is least trustworthy. The mushroom
   button in the drive battery line is the layer for "something is wrong";
   layer 2 is for "it's going too far."

Plus two standing constraints:

- **Hard speed cap, enforced robot-side.** The chassis does 1.3 m/s; a remote
  stranger on unknown latency gets ~0.3 m/s. Clamped in the drive bridge, not
  a pilot setting.
- **The room is the safety sensor.** The base has no cliff/bumper/obstacle
  detection by design, so the demo space is enclosed, has nothing to fall
  off and nothing fragile. Cardboard and tape until it deserves better.

## Control design (pilot)

- **Camera defaults to facing forward while driving.** Drive and look-around
  compete for the operator's mental model; driving with the camera panned 90°
  off-axis is how people drive into walls. Drive input owns the canvas; any
  drive input snaps the PTZ back to its forward park (the `--ptz-home`
  absolute-move machinery exists). Look-around moves behind a modifier key or
  an explicit mode — until v2 gives it to a second person.
- Everything else carries over from DEMO.md: latest-value-wins on the command
  path, stop on pointer/key release, window blur and tab hide, park on
  disconnect (here: heartbeat expiry).

---

## v1 → v2: two cameras, two pilots

**v1** is the rover with the single PTZ on top. Every mechanism above works
with today's stack plus one drive bridge.

**v2** adds a fixed wide-angle **front driving camera** and gives the PTZ to a
**second simultaneous operator** — a driver-plus-spotter pattern real teleop
operations use:

- The **driver** owns the `drive` channel and watches the front camera.
- The **spotter** (a second pilot session) owns the `ptz` channel and looks
  around freely with the top camera while the rover moves.

The hardware is trivial (second camera into the switch). The value of v2 is
that it forces three pieces of *product* work that vehicle customers need
anyway — the demo is the forcing function and the test rig:

1. **Multiple video streams per session, end-to-end.** `seydd` already spawns
   an input per configured video channel (`packages/seydd/src/main.rs`), so
   daemon-side the second camera is a config stanza. The work is wiring video
   channel identity through the session to the pilot rendering two
   `<seyd-video>` surfaces. Real vehicles carry six cameras, not one.
2. **QoS across streams.** The driving camera gets the latency budget and
   bandwidth priority; the look-around camera yields under congestion. An
   extension to the `seyd-qos` controller, and a demo-able differentiator.
3. **Per-channel control ownership in the role model.** Today the cloud
   assigns one driver and the agent drops commands from observers. "Driver
   owns `drive`, spotter owns `ptz`" changes session/role semantics — **this
   needs an ADR when built.**

## Open questions

1. Booking mechanics — calendar link + manually started robot is enough for
   v1; anything more is product work that shouldn't be built for one rover.
2. Front camera model selection (v2): widest H.264 Baseline-capable fixed
   Hikvision at ~$50; confirm `avc1.42001f` so the pilot needs no change.
3. Does the spotter's PTZ session also see the front stream (observer of the
   drive view), or only the PTZ stream? Decide when the multi-stream pilot UI
   exists.
4. Attendant re-arm UX for the latched stop: same phone page as the stop, or
   a physical control on the rover?
