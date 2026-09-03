# ADR 0005 — The pilot paces presentation on the source's capture clock

**Status:** accepted (2026-09-03)

## Context

The pilot decoded on arrival and painted straight from the `VideoDecoder`
output callback — no jitter buffer, no pacing, and the decode timestamp was
`performance.now()` rather than the `capture_ts_us` the wire already carried
(and the reassembler already parsed, then discarded). Every millisecond of
pipeline jitter therefore landed on the glass.

That is not merely untidy: it makes a *structural* artefact visible. A keyframe
is ~8x the payload of a delta frame — 10.4 KB against 1.24 KB on the demo
camera, 14.6 KB against 2.3 KB on the wire once FEC is added — and it is
produced in one frame slot. Once per GOP the picture therefore holds for two or
three frame times and then advances twice within a millisecond. On a continuous
pan an operator reads that as a periodic jerk at exactly the keyframe rate,
which on the demo camera (`GovLength=25` at 25 fps) is 1.00 s.

Measured on the demo camera, panning continuously: the camera itself emits
I-frames on schedule (40.3 ms p50, read directly with ffprobe), but they
reached the pilot 46–122 ms late with the next frame following within ~1 ms,
and there was no loss anywhere (`chunksMissing: 0`, `framesIncomplete: 0`).

A second defect compounded it: `seydd` set `capture_ts_us` to `now_us()`, its
own *arrival* time. The one timeline that could have carried the encoder's
cadence was itself jittery — painted frames showed capture deltas ranging from
3 µs to 43 ms where they should all have been 40000 µs.

## Decision

**`capture_ts_us` is the source's sampling clock.** `seydd` takes it from the
RTP timestamp — `retina`'s `Timestamp::elapsed()` for RTSP, the extended
32-bit RTP timestamp for raw RTP — scaled to microseconds relative to the
stream's first picture. It is not comparable with `send_ts` or the `ping/pong`
clock (ADR 0001).

**The pilot paces presentation on that timeline** (`sdks/js/core/presenter.ts`).
A decoded frame is held until its capture timestamp is due, offset by a delay
the **QoS profile** owns — `pilot_presentation_delay_ms`, carried to the pilot
as `presentation_delay_ms` in `welcome.qos` / `qos-ack.qos`:

| profile | presentation delay | latency budget |
|---|---|---|
| `latency` | 50 ms | 100 ms |
| `balanced` | 100 ms | 100 ms |
| `quality` | 150 ms | 200 ms |

The delay is a latency spend, so it is ordered like every other latency knob in
the profiles and is asserted to stay within the profile's own
`latency_budget_ms`. `presentationDelayMs` on the SDK (and `?pd=` on the demo
page) overrides the profile for one session, for A/B work; 0 restores
decode-on-arrival exactly. A pilot talking to an agent older than this ADR sees
no `presentation_delay_ms` and falls back to 100 ms.

Three properties make it safe:

* **The capture↔pilot mapping is learned, not assumed.** The two clocks share
  no epoch, so the presenter tracks `lag = arrival − capture` and schedules
  against its sliding-window minimum — the fastest transit recently seen —
  slewed rather than jumped.
* **A late burst is drained at the capture cadence**, slightly faster than real
  time, instead of being replayed as fast as it arrived. Replaying the backlog
  is the jump the operator sees.
* **No frame waits longer than the delay past its own arrival.** This bound is
  what makes the delay a genuine budget. Without it, a source whose sampling
  clock runs slower than the pilot's — the demo camera is ~1.6 % slow — walks
  the capture timeline steadily behind real time, and scheduling on it alone
  banks that drift as unbounded latency. An earlier revision did exactly that:
  it removed the judder completely but cost 200–480 ms.

## Consequences

Measured A/B on the demo camera under continuous pan, judder being the
deviation of each paint interval from the ideal 40 ms:

| delay | judder mean | judder p95 | gaps > 120 ms / 40 s | median latency cost |
|---|---|---|---|---|
| 0 (before) | 19.7 ms | 77.4 ms | 61 | — |
| 50 ms | 13.3 ms | 49.0 ms | 50 | +2 ms |
| 100 ms | 8.4 ms | 39.8 ms | 7 | +2 ms |
| 150 ms | 4.1 ms | 14.9 ms | 4 | +3 ms |

No frames are dropped at any setting (`framesShown == framesDecoded`).

The measured latency cost is small **because this camera's clock runs slow**,
so the deadline continuously pulls the schedule forward. On a source with an
accurate clock a frame that arrives on time waits the full delay: budget for
the configured value, not for +2 ms. The delay is bounded by construction, and
that bound is the number to reason about.

The residual hitches at 50 ms are arrival stalls larger than the budget, which
no buffer of that size can hide. Those measurements come from a contended
laptop running camera, agent, cloud and browser together, where arrival p99 was
~190 ms; against the deployed cloud earlier the same day it was ~105 ms.

The default is `balanced`'s 100 ms, which is where the once-per-GOP hitch
actually dies in the measurements above. Because the budget lives in the
profile, changing it is one deliberate product decision per profile rather than
a constant buried in the pilot, and switching profile at runtime re-paces a
live session through `qos-ack`.
