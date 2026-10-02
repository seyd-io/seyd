# ADR 0011 — A publisher states its own bitrate ceiling

**Status:** accepted 2026-10-02.

## Context

The closed-loop rate controller (`seyd-qos::abr`, PLAN.md §1.3) moves the
video bitrate request inside a range whose top is the QoS profile's ceiling:
1.5 Mbps on `latency`, 3 on `balanced`, 6 on `quality`. Its floor is a quarter
of that ceiling and the engine's backlog budget is a few frames at it. All of
that assumes the publisher can follow the request.

The Tello drone demo (DEMO-TELLO.md) was the first publisher that cannot. Its
encoder has five fixed levels, measured at 1, 1.5, 2, 3 and 4 Mbps; nothing
makes it produce more. Under `quality` the controller therefore steered a
range whose top third did not exist: it "raised" the request from 4.7 to 5.2
to 6 Mbps while the stream stayed at 4, its floor sat at 1.5 Mbps where a
quarter of the real maximum is 1, and the backlog budget was sized for frames
half again as large as any the drone sends. The same will be true of any
camera with a firmware bitrate cap, and of simulcast ladders whose highest
rung is modest.

Publisher control is one-way (seydd → publisher, docs/protocol/seydd.md), so a
publisher cannot answer a request with "I can only do 4". The limit is a
property of the encoder that the host knows when it declares the channel.

## Decision

1. **A video channel may declare the most its publisher can encode.**
   `ChannelSpec::max_bitrate_kbps` in `seyd-core`, `max_bitrate_kbps` on a
   `[[channel]]` in `seydd.toml`. Zero, the default, means the publisher has
   no limit of its own, and everything behaves exactly as before.

2. **The effective ceiling is the lower of the profile's and the channel's.**
   `seyd_qos::abr::effective_ceiling_kbps`. The controller is built with it
   (`AbrController::with_ceiling`), so its start point, its ceiling and its
   floor — still a quarter of the ceiling — all scale. A limit above the
   profile's ceiling changes nothing: the profile remains the product's
   ceiling, and this setting can only lower it.

3. **No request exceeds it.** Every `video-config` the agent emits — at
   start, on a profile change, on every controller move — carries a
   `maxBitrateKbps` at or below the effective ceiling. The engine's backlog
   drop budget and the `abr_ceiling_kbps` statistic use the same figure.

4. **It is local to the agent.** The field is not serialised into the
   channel list announced to the cloud or sent to the pilot. No wire format
   and no signal message changes; the pilot's QoS selector still names
   profiles, and a pilot choosing `quality` on a capped robot gets `quality`'s
   pacing and deadlines with the robot's own top bitrate.

5. **It stays a statement by the host, not a measurement by Seyd.** The agent
   does not infer a ceiling from the bitrate it observes: a quiet scene
   legitimately encodes far below the request, and guessing a cap from that
   would lower the ceiling for good. Encoder settings belong to the
   publisher (CLAUDE.md, "Where QoS settings live"); this is the publisher's
   side telling Seyd one fact about them.

## Consequences

* `ChannelSpec` gains a field, which breaks Rust hosts that build the struct
  literally; they add `max_bitrate_kbps: 0`. `seydd` configs are unaffected.
* **The C ABI does not carry it yet.** `seyd_channel_config` is unchanged and
  `seyd-ffi` passes 0, so C, C++, Python and ROS 2 hosts cannot declare a
  ceiling; they can only clamp requests themselves in `on_requested_config`,
  which leaves the controller's floor and budget unscaled. Adding the field
  is an ABI change under ADR 0004 and is deliberately left for when a
  non-daemon host needs it.
* With several video channels, the agent-wide figures (the `abr` statistic,
  the bitrate a profile change resets to) use the tightest declared limit.
* Simulcast is untouched: `activate_above_kbps` thresholds are compared with
  the controller's target as before, which now never exceeds the ceiling — a
  layer whose threshold lies above the declared limit is simply never chosen.

## Alternatives considered

* **Clamp in the bridge only.** The Tello bridge already maps a request to the
  nearest level at or below it, so the stream was never wrong — only the
  controller's picture of it. That leaves the floor, the backlog budget and
  the published statistics wrong, and every such publisher re-solving it.
* **A reverse publisher-control channel** ("I am at 4 Mbps"). More general,
  and the right shape if publishers ever need to report more than a constant;
  for one static number it is a protocol for the sake of a config value.
* **Per-robot custom profiles.** Lets a host change deadlines and pacing as
  well, which is how a profile stops meaning the same thing on every robot.
