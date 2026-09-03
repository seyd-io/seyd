# ADR 0006 — True loss is measured from counters read on one clock

**Status:** accepted (2026-09-03)

## Context

The ABR controller steers FEC and the bitrate ceiling from `pilot_chunk_loss_pct`
— chunk loss measured by pairing the agent's `chunks_sent` against the pilot's
`chunks_rx`. Neither side can measure it alone: a frame whose chunks were *all*
lost is invisible to the pilot, so the pilot's own `chunks_missing` is biased low
by construction.

The agent built that pair itself. On each `pilot-stats` it took the reported
`chunks_rx` and matched it against its own send log as of `now − rtt − 100 ms`
— "only chunks that have had time to arrive". The intent was right; the
execution compared two windows offset by ~100 ms. `d_sent` covered
`[t0−100ms, t1−100ms]` while `d_rx` covered `[t0, t1]`, so whenever the chunk
rate differed between the two ends of the window — a keyframe at one boundary,
a pan enlarging frames — the mismatch appeared as loss. At ~75 chunks/s a
100 ms offset is ~7.5 chunks against a window of a few hundred, which is
percent-level.

The error was symmetric, but the result was `clamp(0.0, 100.0)`. Negative
excursions were discarded and positive ones kept, so zero-mean noise was
**rectified into a steady positive loss signal**.

Measured across the pre-fix runs of 2026-09-03: 118 ABR decisions, of which
**73 reported loss above the controller's 0.2 % FEC threshold, and none of
those had `residual > 0`** — not one genuinely broken frame. Reported loss
reached 26 %. The consequence was FEC stepping up and the bitrate flapping
3000 ↔ 2760 ↔ 2500 kbps on a link losing nothing, and because each ceiling
change is a full-document ISAPI PUT to the camera, 6–22 encoder
reconfigurations per run — each one a visible hitch of its own.

## Decision

The pilot reports the pair it already forms correctly. `StatsTracker` has
always recorded `{sent: agentStats.chunks_sent, rx: this.chunksRx}` at the
instant an `agent-stats` message arrives; `pilot-stats` now carries the newest
such pair as `chunks_sent_seen` / `chunks_rx_seen`, and the agent differences
those across its ≥ 3 s window.

Both halves are then read on one clock, at one instant. Chunks in flight at
that moment still count as sent but not received — but that bias is the same at
both ends of the window and cancels in the difference, which is exactly what the
old time-shift was trying and failing to achieve. The agent's `sent_log` is
deleted, taking a per-frame push under the ABR mutex with it.

A pilot too old to send the pair leaves `loss_pct` unset. The controller then
runs on residual loss and latency alone, which is strictly better than steering
on a number that cannot be computed correctly.

The window arithmetic is now the pure `windowed_loss_pct(span, d_sent, d_rx)`,
with the regression pinned by a test: a clean link whose send rate ramps 60 →
140 chunks/s across the window must read 0 % loss.

## Consequences

Two minutes with a live pilot and the camera panning continuously:

| | before | after |
|---|---|---|
| ABR decisions above the 0.2 % FEC trigger | 73 of 118 | 0 |
| ...of those, with `residual > 0` | 0 | — |
| peak reported loss | 26 % | 0 % |
| camera encoder reconfigurations | 6–22 per run | 1 (the initial profile apply) |
| steady state | flapping 3000/2760/2500 | `steady` at the 3000 ceiling, FEC 15/30 |

The agent's `abr_loss_pct` now agrees with the pilot's `chunksMissing: 0` and
`lossTruePct: 0` instead of contradicting them.

The signal still works, which is the point of the change rather than a
side-effect: with 5 % burst loss injected at the pilot, the agent measures
3.5–8 %, raises FEC to 50/50, cuts the bitrate on residual loss and recovers —
the behaviour `abr.rs` documents.

Both ends must be updated to get the benefit: a new agent with an old pilot has
no pair to use, and an old agent ignores the new fields and keeps its own broken
pairing.
