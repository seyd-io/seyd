# Where the latency is — every stage, traced through the code

Written 2026-09-08 from a read of the Rust core (`packages/`), the daemon
(`seydd`), the browser SDK (`sdks/js/core`, `sdks/js/web`) and the demo. It
lists every place a byte waits between a camera sensor and an operator's
screen, and the reverse path for commands. `docs/latency-roadmap.md` is the
ordered plan; this is the map it works from. Numbers marked **measured** come
from ADRs, PLAN.md or field notes; **estimated** ones are arithmetic on the
constants in the code; **unmeasured** means nothing in the repo has put a
number on it yet.

The single most important finding is about measurement, so it comes first.

## 0. What the HUD's "g2g" actually measures

`Engine.onFrame` computes `g2g = clock.oneWayMs(f.sendTsFirst)` at the moment
the reassembler hands over a complete frame
(`sdks/js/core/src/engine.ts:343`). `send_ts` is stamped in `send_frame`
*after* the frame left the agent's queue and *before* its first chunk was
queued to quinn (`packages/seyd-core/src/engine.rs:633`). So the number the
HUD, `pilot-stats`, `seyd-smoke.py --record` and the field notes call
"glass-to-glass" is:

> first chunk handed to quinn → last chunk received and reassembled in the worker

It **excludes**, on the robot: sensor exposure, encoding, RTP/RTSP delivery to
the agent, depacketisation, and the wait in the agent's frame queue. On the
pilot it **excludes**: the decoder, the presentation delay (50/100/150 ms by
profile, the largest deliberate term in the whole pipeline), canvas paint,
compositor and display scan-out.

Field run A's "p50 17 ms, p95 101 ms" and the double-cellular "31/59 ms" are
therefore network-plus-reassembly figures. Against them the roadmap sets
Guident's audited 52 ms *glass-to-glass*, which is a different quantity. The
true glass-to-glass of the balanced profile is roughly the HUD figure plus
100 ms of presentation delay plus decode and display (see §9 for the sum).
PLAN.md §1.6 already asks for "g2g reported within ±5 ms of an external camera
measurement" and a per-stage breakdown; neither exists yet. Until they do,
every latency claim should say which span it covers.

## 1. Before Seyd sees a byte — the publisher

None of this is Seyd's code, but it is on the path and Seyd can only ask for
it to change (`video-config`, `recovery-request`).

| Source | Mechanism | Magnitude |
|---|---|---|
| Sensor readout and ISP | Rolling shutter plus image pipeline inside the camera | Unmeasured; typically 1–2 frame intervals on IP cameras |
| Encoder lookahead / B-frames | B-frames reorder output and cost at least one frame before the first byte exists; frame threading adds another | Baseline profile on the demo camera has none (DEMO.md, `has_b_frames=0`); the sim sets `-bf 0 -tune zerolatency`. The RPi 5 defaults to both (roadmap item 5). **One frame each when present.** |
| VBV / rate-control burst | The encoder may run over its average rate for the VBV duration; the excess sits in a network queue | Sim: 100 ms VBV (`sim/video-source.sh`). Camera: VBR with a cap, VBV unknown |
| **Keyframe size** | An IDR is ~8× a delta (10.4 vs 1.24 KB payload measured on the demo camera, ADR 0005; 30–60 KB on the sim) and is produced in one frame slot | **Measured:** IDRs reached the pilot 46–122 ms late. This is the structural cause of both the pacing budget (§7) and the p95 tail. Periodic intra refresh instead of IDRs would remove it at the source (roadmap: Voysys does this) |
| GOP length | Sets how long a broken reference chain lasts without a recovery request | 1 s (balanced), 2 s (quality) |
| Camera → agent hop over RTSP/TCP | `packages/seydd/src/input/rtsp.rs` deliberately uses TCP interleaved RTP. A lost segment on a Wi-Fi camera link stalls everything behind it (head-of-line), then releases a burst | **Measured indirectly:** the engine's comment records 10 % of frames skipped by the old single-slot buffer because of "bursty RTSP delivery"; the 6-deep queue (§3) exists to absorb it. The `rtsp inter-frame gap > 120 ms` debug log is the only instrumentation |
| RTP over UDP | No loss recovery at all; a hole in an FU-A discards the fragment (`rtp.rs:98`) and the torn AU still goes out | Loss, not latency |

## 2. Depacketisation — a whole access unit before anything moves

Both inputs emit a complete access unit and nothing before:

- `rtp.rs` accumulates NAL units until the RTP marker bit or a timestamp
  change (`Depacketizer::push`). If a source never sets the marker, the AU is
  only closed by the *next* frame's first packet: **+1 frame interval**.
- retina on the RTSP path does the same (`demuxed()` yields `VideoFrame`
  items, one per AU).

Cost: the serialisation time of the frame on the camera→agent link plus zero
work overlap. On a LAN link this is small for deltas but a 45 KB keyframe on a
camera's 100 Mbps port is still ~4 ms, and on Wi-Fi far more. Roadmap item 4
(sub-frame delivery) removes this stage for the depacketiser half; the FEC
block boundary is what makes the sender half harder.

Two small extra hops before the engine: the per-layer `mpsc::channel(4)`
between input and the push task in `seydd/src/main.rs:174`, and a
`Bytes::copy_from_slice` per frame on the C ABI path (`seyd-ffi/src/lib.rs:650`).
Both are microseconds unless the consumer stalls, in which case the RTP
receive loop blocks on `tx.send().await` and packets pile up in the kernel
socket buffer — latency hiding, not loss, up to the buffer size.

## 3. The agent's frame queue

`FrameQueue` in `packages/seyd-core/src/engine.rs:200` is an in-order queue of
up to `MAX_QUEUED_FRAMES = 6` frames between the input and the single
`frame_sender` task.

- **Depth:** 6 frames is 200 ms at 30 fps or 240 ms at 25 fps of buffering
  before the first drop. It is in-order on purpose (a skipped delta breaks the
  reference chain), so once the sender falls behind, every queued frame adds
  its full interval. A keyframe flushes the queue only when it holds 3 or more
  frames or the chain is already broken (`keyframe_flushes_queue`).
- **Single sender, sequential sessions:** `send_frame` packs once and then
  loops over sessions in order (`engine.rs:653`). For a keyframe whose chunk
  finds quinn's buffer full it sleeps 1 ms and retries up to 50 times *per
  chunk* (`engine.rs:697`), stalling every other session and every frame
  behind it for up to 50 ms per chunk.
- **Admission control is measured on queued bytes (fixed 2026-09-08).** The
  intent (`Profile::drop_threshold_bytes`, "drop a delta if the send backlog
  exceeds this many frame-times", 25 KB for balanced at 30 fps) used to be
  checked as `space < threshold` against quinn's *free* datagram buffer. With
  the buffer at 750 KB that meant ≥ 725 KB — about two seconds of video at
  3 Mbps — had to be queued before a delta was dropped, so up to ~2 s of
  latency could hide inside quinn, and the ABR's *primary* congestion signal
  (`frames_dropped_backlog`, abr.rs rule 4) was effectively silent. The check
  is now `backlog_exceeds(queued, threshold, keyframe_allowance)` on
  `Session::send_buffer_queued()`, and the buffer is 256 KB — sized so a
  keyframe with parity always fits (keyframes are never dropped, and a blocked
  keyframe chunk is abandoned after 50 ms, tearing the frame), not as the
  latency bound. The bound is now the profile's threshold.

  The allowance exists because a keyframe is queued in one go and legitimately
  holds the queue above the threshold while it drains; without it the delta
  after every keyframe would be dropped and *another* keyframe requested — an
  IDR storm on a link that is merely full. For twice the keyframe's
  serialisation time at the current bitrate request its wire size is
  discounted; beyond that a deep queue is real congestion.

  **Verified through a shaped link the same day, and it found a second
  problem.** With the check live, a 4.5 Mbps link at 46 ms RTT produced a
  burst of 21–28 dropped deltas exactly every 10 s, each followed by a
  700–900 ms freeze while the sender waited for a keyframe. The 10 s period is
  BBR's ProbeRTT (§5): quinn shrinks the window to 0.75 × its bandwidth-delay
  estimate for 200 ms, the link runs at three quarters of the video rate, and
  the queue crosses a two-frame threshold every time. The old check never saw
  it because it never fired at all. The backlog must therefore be *sustained*
  for `BACKLOG_SUSTAIN` = 300 ms before a delta is dropped
  (`backlog_sustained`); re-measured, all four runs showed zero drops. A queue
  that outlives the probe is congestion; one that does not is the controller
  measuring the path, and the drop would cost a freeze to save 200 ms of queue
  that drains by itself. See §11 for the numbers.

## 4. Packing and FEC

`pack_video` (`packages/seyd-core/src/packer.rs`) is whole-frame: the AU is
copied once if frame meta is prepended, split into 1000-byte chunks in blocks
of 8, and each block's parity computed over zero-padded copies.

- **Chunk length is fixed at 1000 bytes.** `Agent::start` passes
  `DEFAULT_CHUNK_LEN` (`agent.rs:156`) and nothing ever raises it, although
  DPLPMTUD runs and `current_mtu` is reported in `agent-stats`. The wire
  header allows 1350. At 1000 a 45 KB keyframe is 45 data chunks plus parity
  instead of 34; every chunk costs a pacer token and a per-packet overhead of
  ~60 bytes. PLAN.md §1.4 lists raising it as done-in-principle; it is not.
- **FEC overhead lengthens serialisation.** Bytes on the wire are payload ×
  (1 + FEC %): 15/30 % (balanced), 25/50 % (latency — note the latency
  profile pays the *most* parity on keyframes), rising to 50/50 under loss.
  A 45 KB IDR at 50 % is ~68 KB on a ~2 Mbps uplink: 270 ms instead of
  180 ms. This is the trade the profile makes, but it is a latency term.
- **Encode CPU.** `seyd-fec` is scalar GF(256) table lookups (`gf::madd_into`);
  PLAN.md §1.7's SIMD version is not built. Estimated sub-millisecond per
  keyframe on x86, low milliseconds on a Raspberry Pi. Unmeasured.
- **Sub-frame delivery** (roadmap item 4): because a block can only be
  emitted once its 8 data chunks exist and the frame is packed whole, the
  first chunk of a frame leaves after the *last* byte arrived. Up to one frame
  interval, largest for keyframes.

## 5. quinn: the transport is not a plain UDP socket

`Session::send_datagram` (`seyd-transport/src/session.rs:101`) appends to
quinn's datagram queue, which is **FIFO** (`quinn-proto` `datagrams.outgoing`
is a `VecDeque`). What happens after that is decided by quinn, and three of
its mechanisms add latency that the engine cannot see:

1. **Congestion window.** Application datagrams are ack-eliciting packets and
   are gated by `in_flight + packet ≥ cwnd` exactly like stream data
   (`quinn-proto connection/mod.rs`, "blocked by congestion control"). With
   BBR the window is about 2 × bandwidth-delay product. At 3 Mbps and 25 ms
   RTT that is ~19 KB, so a 45–68 KB keyframe **cannot leave in one RTT**
   regardless of how fast the physical link is: it drains over 2–4 RTTs as
   ACKs return. On a LAN the link could carry it in 4 ms; BBR's bandwidth
   estimate is capped by the application's own send rate (app-limited), so
   the window never grows to cover a burst 8× the average. This is a plausible
   contributor to "IDRs reached the pilot 46–122 ms late" (ADR 0005) that no
   document names. `cwnd` is in `agent-stats`; if it sits near 2 × BDP while
   keyframes are late, this is confirmed. The knobs are quinn's
   `BbrConfig::initial_window` and, more fundamentally, whether media
   datagrams should be cwnd-gated at all (they are not retransmitted, and
   Seyd already does its own admission control).
   **ProbeRTT (measured).** Every 10 s, when not app-limited, quinn's BBR
   enters ProbeRTT and holds the window at 0.75 × BDP for 200 ms
   (`quinn-proto` `congestion/bbr/mod.rs`, `PROBE_RTT_BASED_ON_BDP`, not
   configurable). At 46 ms RTT that throttles a 3.4 Mbps stream to about three
   quarters of its rate for a fifth of a second: ~20–25 KB of queue and an RTT
   spike from 58 to ~110 ms, once every 10 s, with nothing wrong on the link.
   Harmless on its own; it became a freeze only through admission control (§3),
   which now ignores a backlog shorter than 300 ms.
2. **Pacer.** A token bucket refilling at 1.25 × cwnd per RTT
   (`quinn-proto connection/pacing.rs`) spreads a burst over most of an RTT.
   Protective on a modem, pure delay on a fast path.
3. **Send buffer depth.** 256 KB (§3), but what is queued in it is now capped
   by admission control at the profile threshold plus one keyframe.

Minor: datagrams are written into a packet *before* stream frames
(`populate_packet`), so the control stream (pings, `loss`, `request-keyframe`)
can wait behind a keyframe burst, and so can **sensor datagrams**, which
share the FIFO with video chunks — a sensor message queued behind 60 keyframe
chunks waits for the whole keyframe. Initial MTU 1200, keep-alive 2 s and
idle 10 s do not affect steady-state latency.

## 6. The network

RTT and its inflation are measured and used (BBR, ABR gate, clock offset);
the path itself is outside the code. What the code adds on top of it:

- **Loss → recovery chain.** A chunk the parity cannot cover is only known
  lost when either the *next* frame decodes (closing older frames,
  `reassembler.ts:199`) or the silence timer fires (20/30/50 ms delta by
  profile, adaptive up to 250 ms on jittery paths, `effectiveDeadline`). Then
  `loss` goes up the control stream, the agent's ladder decides
  (`request_recovery`, rate-limited to one per 250 ms per channel), the daemon
  writes UDP to the publisher, the demo bridge issues an ISAPI HTTP PUT
  (`hikvision.py`, 1.5 s timeout, thread pool), the camera emits an IDR at its
  next slot, and that IDR takes the full pipeline including §5. **Estimated
  chain: 1 frame + 1 RTT + publisher reaction (tens to hundreds of ms on the
  camera) + IDR serialisation.** Between loss and recovery the picture is
  frozen or smeared (`on_loss` policy). The ladder's cheaper rungs (`ltr`,
  `intra_refresh`) are only cheap if the publisher honours them; the demo
  bridge maps every request to an IDR.
- **`recovery_grace_ms`** (700/900/1400) is how long the sender holds deltas
  after a non-IDR recovery request. On a publisher that ignores the request
  this is a visible freeze of that length.

## 7. The pilot — reassembly, decode, presentation, paint

### Reassembly (`sdks/js/core/src/reassembler.ts`)

No added latency on the clean path: a block completes the instant `n` chunks
of it exist, and a frame is handed over the instant its last block completes.
Data is copied three times on the way (per-chunk body, per-block `out`,
per-frame `buf`) and a fourth time into `EncodedVideoChunk`; microseconds at
these sizes.

### Decoder (`sdks/js/core/src/decoder.ts`)

`VideoDecoder` with `optimizeForLatency: true` and hardware
`'no-preference'`. Two things the code does not control:

- **Decoder output delay.** Hardware decoders (VideoToolbox, D3D11, V4L2)
  commonly hold one or more frames before output, and any decoder may hold
  frames for reordering unless the SPS VUI carries
  `bitstream_restriction_flag` with `max_num_reorder_frames = 0`. x264 with
  `zerolatency` writes it; IP cameras often do not, and Baseline profile alone
  does not tell the decoder there are no B-frames. Unmeasured: the gap between
  `decode()` and the `output` callback is not instrumented (`decodeQueueSize`
  is, and is the proxy to watch).
- **No back-pressure before the decoder.** If decoding falls behind (software
  decode of 720p on a weak laptop, or a hardware decoder queue), `decode()`
  keeps enqueueing; latency grows without bound until the presenter's
  `MAX_QUEUE = 32` collapses *decoded* frames. `decode_q` is sent in
  `pilot-stats` and the agent ignores it; PLAN.md §1.6 wanted it as an ABR
  signal.

MJPEG (`mjpeg.ts`) decodes each frame with `createImageBitmap`, serialised on
a promise chain: full decode time per frame, no pipelining, unmeasured.

### Presentation pacing (`sdks/js/core/src/presenter.ts`, ADR 0005)

**The largest deliberate term.** A decoded frame is held until
`capture_ts + minLag + delayMs`, with `delayMs` = 50 / 100 / 150 ms by profile
(`seyd-qos/src/lib.rs`). It exists to hide the once-per-GOP keyframe hitch
(§1). Properties that matter for latency:

- A frame that took the fastest recent path waits the full delay; ADR 0005's
  "+2 ms measured cost" was on a camera whose clock runs 1.6 % slow, which
  continually pulled the schedule forward. **Budget the configured value.**
- After a stall the backlog drains at `CATCHUP = 0.9` (11 % faster than real
  time): a 200 ms stall takes about two seconds to bleed back out. Bounded by
  "never later than `delayMs` past arrival".
- The schedule is a `setTimeout` chain in the worker, painting at timer edges,
  not at vsync (see below).
- The whole budget exists because IDRs are 8× a delta. With periodic intra
  refresh at the encoder (no periodic IDRs, roadmap "Voysys" note) the pacing
  delay could drop toward one frame interval on the latency profile.

### Paint and display

`paint()` draws with `drawImage` on an `OffscreenCanvas` 2D context in the
worker (`engine.ts:380`). The commit reaches the compositor on its next
frame: **0–16.7 ms (mean ~8 ms) at 60 Hz**, then one more display refresh to
scan out. Neither is instrumented and neither is aligned with the presenter's
timers, so the paced schedule is quantised by the display anyway. Roughly
**10–25 ms** of the pipeline sits here on a 60 Hz panel, less on 120 Hz.

### Join latency

Signaling WebSocket → `offer` → candidate race (`race.ts`, `needs_probe`
candidates held 400 ms, deadline 2–10 s by hint) → QUIC + TLS handshake →
`hello`/`welcome` (1 RTT) → `request-keyframe` → publisher IDR → pipeline.
DEMO.md records this as solved by requesting a keyframe on `hello`; the
publisher's reaction time is the remaining unknown.

## 8. The control loop's own latency

Adaptation is slow by design, and the delay is part of the latency picture
because a congested link holds a queue until the controller acts:

| Step | Interval | Where |
|---|---|---|
| Measurement | 1 Hz `pilot-stats` / `agent-stats`, ≥ 3 s loss window | `engine.rs` `abr_loop`, `note_pilot_stats` |
| RTT gate | `rtt − min_rtt > budget/2` (50 ms) for 2 consecutive seconds, keyframe-polluted samples skipped | `abr.rs` rule 4 |
| Bitrate emission | at most every 5 s, ≥ 10 % moves | `abr.rs` rule 5 |
| Publisher apply | demo bridge coalesces to one ISAPI write per 2 s; the camera applies at its own pace | `bridge.py` `coalescer` |
| FEC | up within 1 s, down after 10 clean seconds | `abr.rs` |
| Simulcast switch | armed on a tick, lands on the target layer's next keyframe; up to a GOP unless the publisher forces an IDR | `engine.rs` `push_video_layer` |

So from the onset of congestion to a lower bitrate at the encoder is
**≈ 3–10 s**, during which 50 ms of standing queue is tolerated by design and
since 2026-09-08 the backlog drop fires at the profile threshold and cuts
the bitrate within the next ABR tick (§3).

## 9. Adding it up — the balanced profile, clean link, 25 fps camera

Estimated per stage; the only measured spans are marked. Everything else is
what this document argues should be instrumented next.

| Stage | Typical | Worst common case |
|---|---|---|
| Sensor + encoder (camera) | 1–2 frames (40–80 ms) | + B-frame/threading on a misconfigured encoder |
| Camera → agent (RTSP/TCP, LAN) | ~1–5 ms | tens of ms after a TCP retransmit |
| Whole-AU depacketisation | ≈ serialisation of the frame on that link | 1 frame if the marker bit is missing |
| Agent frame queue | ~0 when keeping up | up to 6 frames (240 ms) |
| Pack + FEC | < 1 ms (x86) | low ms (Pi) |
| quinn cwnd + pacer + buffer | ~0 for deltas | multiple RTTs for a keyframe; queued bytes bounded by the profile threshold plus one keyframe |
| Network one-way | path RTT / 2 | + modem queue |
| **send → reassembled (the HUD "g2g")** | **measured 17 ms p50 (run A), 31 ms double-cellular** | **101 / 59 ms p95** |
| Decoder | 1–few ms software; 1+ frames on some hardware paths | unbounded queue growth when CPU-bound |
| Presentation delay | **100 ms** (balanced), 50 (latency), 150 (quality) | + up to 2 s catch-up after a stall |
| Canvas → compositor → display | ~10–25 ms at 60 Hz | |

Reading across: on the balanced profile the pilot-side deliberate and display
terms alone (100 + ~15 ms) exceed the entire HUD "g2g" p95 of the best field
run, and the camera's own encoder delay is likely of the same order again.
The realistic glass-to-glass of the current stack is therefore in the region
of **200–250 ms median on balanced, ~150 ms on latency**, not 17 ms — which is
what an external camera-and-stopwatch measurement (PLAN.md §1.6) would show,
and what has to be measured before comparing with Guident's 52 ms.

## 10. Highest-leverage changes, in order

1. **Measure the real thing.** Timestamp each stage (`capture_ts` is already
   on the wire; add agent ingest time, decode output time and paint time to the
   trace) and do one external glass-to-glass measurement per profile. Everything
   below is guesswork until this exists.
2. ~~**Fix the admission check** (§3)~~ — done 2026-09-08: backlog is measured
   on queued bytes with a keyframe allowance, and the buffer is 256 KB. Still to
   do: confirm on a throttled link that `frames_dropped_backlog` moves and the
   ABR cuts on `backlog`.
3. **Periodic intra refresh at the publisher, no periodic IDRs.** Removes the
   keyframe burst that drives §1, §4, §5 and the reason for §7's budget. The
   protocol already has the rung. **Done in the sim 2026-09-08 and measured
   (§11):** through a shaped 4.5 Mbps / 40 ms link, g2g p95 148 → 48 ms and
   arrival gaps over two frames 41 → 8 per 40 s, the 8 being the sim's forced
   IDRs. The camera still emits IDR GOPs; whether it can do intra refresh is
   the next thing to find out.
4. **Lower the presentation delay once (3) lands on the camera**, per
   profile, and re-run ADR 0005's judder table. On the sim with intra refresh,
   decode-on-arrival already paints with 4.5 ms mean judder and its only gaps
   are the forced IDRs.
5. **Investigate cwnd gating of datagrams** (§5) with the `cwnd` stat before
   touching quinn's configuration.
6. **Raise `chunk_len` from PMTUD** (§4): fewer packets, fewer pacer tokens.
7. **Sub-frame delivery** (roadmap item 4) and SIMD FEC (PLAN §1.7).

## 11. Measured 2026-09-08: intra refresh against IDR GOPs

Setup, reproducible with what is in the repo: `sim/video-source.sh` with
`VIDEO_DEVICE=lavfi` (testsrc2, 1280×720 at 30 fps, balanced profile,
3 Mbps cap) in both modes — `INTRA_REFRESH=1`, the new default, with a forced
IDR every 5 s because FFmpeg cannot emit one on demand, and `INTRA_REFRESH=0`
for the classic 1 s IDR GOP; `seydd` with `host_override = "127.0.0.1"`;
`tools/link-shaper.py` at 4.5 Mbps, 200 KB queue, 20 ms one-way delay in
front of it; `tools/latency-ab.py` recording 40 s per run in headless Chrome
after a 6 s warm-up. Zero chunk loss in every run. "g2g" is the HUD's span
(§0), not glass-to-glass; "gaps" counts intervals longer than two frames.

| | IDR GOP | intra refresh | IDR GOP | intra refresh | IDR GOP | intra refresh |
|---|---|---|---|---|---|---|
| presentation delay | 0 | 0 | 50 ms | 50 ms | 100 ms | 100 ms |
| keyframes in 40 s | 41 | 8 (forced) | 41 | 8 | 41 | 8 |
| g2g p50 / p95 / p99 (ms) | 42 / 98 / 139 | 42 / 46 / 78 | 38 / 86 / 98 | 44 / 52 / 205¹ | 45 / 148 / 181 | 44 / 48 / 83 |
| arrival gaps > 2 frames | 41 | 8 | 41 | 10 | 41 | 8 |
| paint judder mean / p95 (ms) | 7.3 / 14.0 | 4.5 / 10.1 | 2.2 / 4.2 | 1.7 / 3.7 | 0.8 / 2.1 | 0.5 / 1.4 |
| paint gaps > 2 frames | 41 | 8 | 2 | 3 | 2 | 0 |

¹ One isolated 325 ms stall in that run, not periodic and with no backlog
drop; the p95 is the representative figure.

What the table says:

- **The IDR burst is the tail.** With IDR GOPs every one of the 41 keyframes
  arrives more than two frames after its predecessor, at any presentation
  delay; g2g p95 is 2–3× the median. With intra refresh the tail collapses to
  the forced IDRs, and between them p95 sits within 5 ms of the median.
- **The presentation budget is what hides it.** At 0 ms the IDR stream paints
  41 visible hitches in 40 s; at 100 ms, two. Intra refresh at 0 ms paints 8,
  all at forced IDRs, which a publisher honouring on-demand IDR would not
  emit. That is the case for lowering the budget once the publisher stops
  sending periodic IDRs, exactly as §7 argued.
- **On loopback none of this is visible.** The same A/B without the shaper
  showed both modes at ~2 ms judder and sub-4 ms g2g p95: a 35 KB frame costs
  nothing on an unconstrained link, which is why the shaper exists.
- **Caveat on magnitude.** testsrc2 compresses badly, so its IDRs are only
  3.3× a delta (34 KB against 11 KB); the demo camera's are 8×. The effect
  on a real scene is larger than measured here.
- **Found along the way:** the BBR ProbeRTT interaction (§3, §5), which only
  the fixed admission check could expose and which the sustain guard removes.

## Command path (pilot → robot), for completeness

Operator input → `ptz.ts` (sends on change, repeats every 200 ms) →
`postMessage` to the worker → `encodeMessage` datagram (`engine.ts:395`) →
QUIC → agent `dg_task` → `events` channel (bounded 256, awaited) →
`maintain` loop → host channel → `seydd` event loop → UDP → `bridge.py` →
ISAPI HTTP PUT in a thread pool → camera motor. Seyd's hops are sub-millisecond
each; the camera's HTTP round trip and its `momentary` 600 ms motion window
dominate. One structural hazard: the `maintain` loop awaits signaling calls
(`set_sessions`) in the same `select!` that forwards commands, so a slow cloud
call on session start delays command delivery for its duration. Also worth
knowing: the bridge discards commands whose wall-clock `ts` is more than
1.5 s from its own clock, so an unsynchronised robot clock silently drops
every command.
