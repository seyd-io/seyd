# Closing the gap to the best teleoperation link

Ordered work on end-to-end latency, written 2026-09-05 after a codec and
competitor survey. Items 1–3 are **done**; the rest are specified here so the
reasoning survives.

## Where we actually stand

Field run A (2026-08-30, pilot on an iPhone hotspot, robot on the office LAN):
**p50 17 ms, p95 101 ms**, 25 fps at ~1.7 Mbps, 0.0 % true chunk loss.

The published bar, from the September 2026 survey:

| | Glass-to-glass | Source |
|---|---|---|
| Guident | 52 ms average across 40 sites on 4G/5G; 40 ms on private LTE | SEC S-1/A, 2026-03-13 — the only audited figure in the field |
| Voysys | < 45 ms over cellular | Vendor claim |
| DriveU | 100 ms stated as the *requirement* | Vendor blog, 2021 |

**Our median already beats every published figure. The entire gap is the
tail**, and PLAN.md already named the cause: 30–60 KB IDRs serialising over a
~2 Mbps uplink. Aim at ~50 ms p95, not ~150 ms.

Worth knowing: Voysys — ten years old, profitable — runs H.265 with **periodic
intra refresh**, a proprietary UDP protocol explicitly not WebRTC, cloud for
handshake and presence only, and primary streams peer-to-peer. That is this
architecture, arrived at independently. The rung we were missing is the one
they built their product on.

---

## Done

### 1. The recovery ladder — `ltr` → `intra_refresh` → `idr`

`request_recovery` asked for `idr` unconditionally. It now starts at the
cheapest rung and climbs only when a rung has had the profile's
`recovery_grace_ms` and loss continues, resetting after two quiet seconds.
`request-keyframe` from a pilot still forces `idr` and sits outside the ladder.
See `docs/protocol/seydd.md`.

Two things only showed up by running it against a real pilot, both now fixed:

- **The pilot forced every loss to an IDR.** On an unrecoverable frame it both
  called `requestRecovery()` *and* sent `loss`; both reached the agent inside
  its 250 ms rate limit, the keyframe request won every time, and the ladder
  was unreachable. The pilot now reports the loss and lets the agent choose.
- **`pilot-request` poisoned the ladder state**, recording `idr` as the current
  rung, and since IDR escalates to itself the cheaper rungs never came back.

Measured under 6 % sparse loss after the fixes: 2× `ltr`, 2× `intra_refresh`,
4× `idr` from escalation, 2× `idr` from `request-keyframe`.

### 2. Streams that never produce a keyframe

A publisher answering with `ltr` or `intra_refresh` sends no keyframe. Both
`skip_until_key` latches — the input queue's and each session's — cleared only
on `frame.keyframe`, so such a stream **latched silent for the rest of the
session**. Not a degradation: a total stall.

Both latches now carry a deadline from the QoS profile's `recovery_grace_ms`
(700 / 900 / 1400 ms for latency / balanced / quality). It is a profile
tunable rather than a constant because the trade it makes — how long a freeze
is tolerated against how eagerly deltas resume into a broken chain — is exactly
what a QoS profile is for.

This matters beyond our own ladder: Axis Zipstream ships dynamic GOP **enabled
by default**, and `retina` lists periodic intra refresh as unimplemented for
H.264. Sources with sparse or absent IDRs are ordinary in the field.

### 3. MJPEG ingest

Shipped 2026-09-05. `Codec::Mjpeg` on the RTSP path (retina reconstructs the
JFIF headers RFC 2435 strips), and `MjpegDecoder` in `@seyd/core` decoding via
`createImageBitmap` because MJPEG is not a WebCodecs codec. Verified end to end
against a live MJPEG RTSP source: 120 frames / 120 keyframes / 0 loss in 8 s at
the source's 15 fps, then decoded in headless Chrome at 640×480.

Bare `rtp://` MJPEG is refused with an explanation — RFC 2435 header
reconstruction is retina's job on the RTSP path and is not duplicated. The RFC
also mandates standard Huffman tables, which is a real interop trap: FFmpeg
optimises them by default and the packetiser then refuses the stream outright.

Measured bandwidth: 640×480@15 MJPEG = 3478 kbps against 1280×720@30 H.264 =
3431 kbps — the same link for an eighth of the pixels. `seydd` warns on every
MJPEG channel, and it ships as a compatibility path, never a default.

Why it was worth building despite that cost: ONVIF **Profile S makes MJPEG
mandatory** while H.265 is only conditional, so every conformant camera can
emit it and none is obliged to emit anything better; Voysys accepts it on three
input paths; and it is everywhere in robotics (`RS2_FORMAT_MJPEG`, the OAK
encoder, `usb_cam`'s default `mjpeg2rgb`).

### 3b. Admission control measured on queued bytes

Found while mapping every latency source (`docs/latency-sources.md`, 2026-09-08).
`send_frame` dropped a delta when quinn's *free* datagram space fell below the
profile's backlog threshold — but the buffer was 750 KB, so ≥ 725 KB (about two
seconds of video at 3 Mbps) had to queue up first. Two effects: up to ~2 s of
latency could hide inside the transport, and `frames_dropped_backlog`, the
ABR's primary congestion signal, was effectively never raised, leaving only the
two-second RTT gate.

Now `backlog_exceeds(queued, threshold, keyframe_allowance)` on
`Session::send_buffer_queued()`; the buffer is 256 KB, sized only so that a
keyframe with parity always fits. The allowance discounts a just-sent keyframe
for twice its serialisation time at the requested bitrate, because a keyframe
legitimately holds the queue over the threshold while it drains and dropping
the delta behind it would ask for another keyframe. Unit-tested; the field
check — `frames_dropped_backlog` rising and the ABR cutting on `backlog` on a
throttled link — is still to run.

---

## Next

### 4. Sub-frame delivery — medium

`pack_video` takes a complete access unit and the RTP depacketizer accumulates
until the marker bit, so a frame is fully received, then chunked, then sent.
Emitting chunks as NAL units arrive saves up to one frame interval — 33 ms at
30 fps, 16 ms at 60 — and saves most on large keyframes, which is where it
hurts. The depacketizer half needs nothing from the customer.

The design question is FEC block formation: parity cannot be computed over a
block that is not yet complete, so smaller blocks trade protection efficiency
for latency. This does **not** conflict with "whole frame or nothing", which is
a drop policy, not a send policy.

### 5. Encoder preflight and warnings — small

Detect and report bad encoder configuration at ingest: B-frames (visible as
timestamp reordering), GOP length, keyframe size against bitrate. The
Raspberry Pi 5 ships B-frames and frame threading **by default** and has no
hardware H.264 encoder at all, so a large slice of the market arrives
misconfigured, and one B-frame adds a frame of latency before Seyd sees a byte.
Axis' own documentation warns that dynamic GOP "might need clients to adapt".

Pairs with a one-page "configure your encoder like this" document, which is
plausibly the cheapest latency improvement available to us — most of the
encoder-side wins are the customer's to make, and we can only ask.

### 6. Continuous glass-to-glass measurement — medium

p50/p95/p99 per session, in the console, over time. One field run and a smoke
test is not enough to see a tail, and it is how items 3–5 get proven.

### 7. Multipath / link bonding — large ⚠️

Two links, duplicate or split, first arrival wins. Removes the cellular fade
tail, which is the last structural gap to best-in-class.

**Patent exposure to clear first:** Voysys holds WO2021110863A1 on multi-link
redundancy; Ottopia holds US12308959B2 (application-level FEC) and
US11438264B2 (predictive ABR). Get an opinion before building, not after.

Do field-test run B — robot on the hotspot rather than the pilot, still
outstanding in PLAN.md — before committing. That is the configuration
customers will actually have.

### 8. MPEG-TS demux and KLV — medium, conditional

**Only if we go after drones or defence.** There it is not optional: MISP-2025.1
(and so STANAG 4609) mandates MPEG-2 TS as the container, and QGroundControl
and Auterion Mission Control both list MPEG-TS as a first-class video source.
Mature Rust crates exist (`mpeg2ts-reader`).

The real prize is not the video but the **KLV metadata** on the same transport
(MISB ST 1402.2). For an ISR customer, video without KLV is useless. If the
market turns out to be defence, this moves to the top of the list.

---

## Corrections to carry

- **H.265 Main 10.** MISB mandates 10-bit HEVC up to Level 5.1. Our
  depacketizer is byte-level and does not care, but the codec *string* does —
  `hev1.1.…` is 8-bit Main; Main 10 is profile 2.
- **The no-B-frames rule is not a Seyd quirk.** Foxglove's `CompressedVideo`
  schema states it independently, and MISB ST 0804 bans interleaved RTP mode
  for the same latency reason. It belongs in ADR 0005, not only in DEMO.md.
- **Not worth building:** codec work beyond H.264/H.265/MJPEG. AV1 has
  essentially no hardware encoder in the robot world and appears in neither
  ONVIF nor MISB. VP8/VP9 are emitted by nothing in the survey.
