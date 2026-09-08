# ADR 0008 — Simulcast: adapt resolution by selecting a stream, not by reconfiguring one

**Status:** accepted (2026-09-08)

## Context

Under congestion the ABR (ADR 0006, `seyd-qos`) lowers a bitrate ceiling and the
publisher decides how to spend fewer bits. Until now the only rung Seyd could
actually pull on the demo camera was the bitrate cap itself, at a fixed
1280x720, plus a frame-rate cut once pinned at the floor.

Frame rate is the wrong first move. For a remote pilot **frame cadence is a
latency term, not a quality term**: 25 → 15 fps stretches the interval between
frames from 40 ms to 67 ms, so the operator waits up to 27 ms longer to see the
consequence of their own input, against a measured glass-to-glass p50 of ~10 ms.
Resolution should go first and frame rate last.

Three measurements on the demo camera (Hikvision DS-2DE2A404IWG1-E, V5.9.5)
shaped the decision:

1. **The main stream cannot go lower.** Channel 101 advertises widths
   `1280, 1920, 2560`. The demo already runs at 1280x720 — that channel's
   floor. There is no resolution rung on it at all.
2. **Reconfiguring resolution does not affect the running stream.** Writing
   `videoResolutionWidth`/`Height` returns 200 and is stored, but the live RTSP
   session continues at the old resolution (verified on channel 103, 704x576 →
   640x480: frame delivery continued unbroken at 9.5 fps across the change).
   Only a *new* session gets the new resolution. So a resolution rung costs an
   RTSP reconnect — ~83 ms measured, plus a pilot decoder reconfigure — incurred
   precisely when the link is already in trouble.
3. **The camera already encodes several streams simultaneously, for free.**
   Channels 101/102/103 are independent encoders over the same sensor image,
   each with its own resolution, bitrate, frame rate and codec, all running
   whether or not anyone reads them.

Point 3 makes point 2 avoidable.

## Decision

A video channel may declare more than one **layer**. Each layer is an
independently encoded stream of the same picture at a different operating point.
The agent subscribes to every declared layer and **relays exactly one**,
switching between them at a keyframe boundary. Nothing is decoded, re-encoded or
inspected beyond the framing Seyd already does — the agent chooses which
already-encoded bytes to forward, which is the "no transcoding" principle
(CLAUDE.md) applied to adaptation rather than an exception to it.

Selection is policy and lives in `seyd-qos`
(`seyd_qos::simulcast::select`): a pure function of the declared layers, the
ABR's current target bitrate and the currently active layer. It is separate from
the `AbrController` because the controller decides *how many bits*, and this
decides *which encoding fits them* — a robot with one layer runs the same
controller unchanged.

The switch itself is in `seyd-core`. Rules:

1. **Switch on a keyframe of the target layer, never mid-GOP.** Frames of the
   non-active layer are dropped at ingest, before the frame queue, so the
   queue's admission control and "whole frame or nothing" invariant are
   untouched.
2. **Ask for that keyframe rather than waiting for it.** Selecting a new layer
   raises a recovery request naming that layer, so a publisher that can force an
   IDR (`requestKeyFrame` on the demo camera) makes the switch immediate instead
   of waiting up to a GOP.
3. **A layer switch is not a loss event.** It must not raise the FEC level or
   trip the recovery ladder; the deltas discarded belong to a stream nobody is
   watching any more.
4. **Hysteresis is mandatory.** A layer change costs the pilot a decoder
   reconfigure, so upward moves require the target to be comfortably affordable
   and are rate-limited. Flapping is worse than sitting one rung low.

### API and wire impact

The channel descriptor gains an optional `layers` array, so a single-layer robot
serialises and behaves exactly as before:

```jsonc
{ "id": 1, "kind": "video", "name": "main", "codec": "avc1.42001f", "fps": 25,
  "layers": [ { "id": 0, "name": "low",  "activate_above_kbps": 0 },
              { "id": 1, "name": "high", "activate_above_kbps": 1800 } ] }
```

This is additive on `announce`/`welcome`; a pilot that ignores `layers` is
correct, because the change a switch produces is already in-band. Inputs are
Annex B with parameter sets inline on every keyframe, so the new SPS travels
with the keyframe that begins the new layer, and the pilot's renderer already
resizes on a `displayWidth`/`displayHeight` change
(`sdks/js/core/src/engine.ts`). The pilot is told which layer is live through
the stats it already receives, for the HUD and for field diagnosis.

**The channel's `codec` string must cover the highest layer.** The pilot
configures its `VideoDecoder` once from that string, so a channel whose top rung
is 1920x1080 must declare a level that admits it — `avc1.42001f` is Baseline
level 3.1, whose 3600-macroblock limit is exactly 1280x720, so 1080p needs
`avc1.420028` (level 4.0). All layers must be the same codec family; `seydd`
rejects a configuration that mixes them.

C ABI additions are appended under ADR 0004 rule 3, so `SEYD_ABI_VERSION` stays
1: `seyd_channel_add_layer`, `seyd_push_frame_layer`, and `on_layer_changed` in
the callbacks struct. `seyd_push_frame` remains valid and means layer 0.

## Alternatives rejected

* **Reconfigure one stream's resolution.** Costs an RTSP reconnect per rung
  (~83 ms) exactly when the link is worst, and on the demo camera's main stream
  there is no lower resolution to reconfigure to.
* **Cut frame rate first.** Cheaper to apply — `maxFrameRate` takes effect on
  the running stream with no reconnect — but it spends the budget the product
  exists to protect. Kept as the last rung, below every resolution step.
* **Scalable coding (SVC).** One stream carrying droppable enhancement layers
  avoids the switch entirely, but no ONVIF camera emits it, and requiring it
  would exclude every device a customer already owns.

## Consequences

* Adaptation costs no reconnect and no encoder restart, so the pilot sees a
  resolution change as one keyframe.
* The robot receives every layer and forwards one, so the camera-to-agent link
  carries the sum of the layers. That is free on a LAN or a direct cable and is
  the reason simulcast is a robot-side feature rather than an uplink one; the
  uplink still carries exactly one stream.
* A publisher that cannot produce multiple streams declares one layer and
  nothing changes for it.
* `sim/` gains a second FFmpeg output so the simulation exercises the same path
  as the camera; the demo's ladder is documented in DEMO.md.
