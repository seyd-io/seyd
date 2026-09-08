# ADR 0009 — Keyframes on demand: intra refresh or a long GOP, never a one-second IDR cadence

**Status:** accepted (2026-09-08)

## Context

Every QoS profile asked the publisher for `maxGopMs` of 1000 (2000 for
`quality`), and the demo camera and the simulation both obliged with an IDR
every second. An IDR is several times a delta frame — 26 KB against 1.1 KB on
the demo camera's static scene, 34 KB against 11 KB on the sim — and it is
produced in one frame slot. The consequences were traced stage by stage in
`docs/latency-sources.md` and then measured:

- **It is the tail.** Through a shaped 4.5 Mbps / 40 ms link the sim's IDR
  stream had a g2g p95 of 148 ms against a 45 ms median, and every one of the
  41 keyframes in 40 s arrived more than two frame intervals after its
  predecessor (§11).
- **It is the hitch ADR 0005 exists to hide.** On the demo camera, with no
  network in the way at all, each keyframe reached the agent about 22 ms after
  its slot, so once a second the frame interval read 60 ms then 18 ms: 168
  such steps in 40 s. The 100 ms presentation delay was sized to absorb
  exactly this.
- **It is bandwidth.** On the camera's static scene the periodic IDRs were 43 %
  of the stream (696 → 394 kbps when they stopped).
- **It is not needed.** The agent already asks for a recovery point whenever a
  pilot needs one — on `hello`, on unrecoverable loss, on a backlog drop, on a
  layer switch — and the demo camera answers `requestKeyFrame` with an IDR
  96–155 ms later, measured at both GOP lengths. The periodic IDR was doing
  nothing a request does not do better, and doing it once a second whether or
  not anyone needed it.

Two encoder strategies remove the burst. **Periodic intra refresh** intra-codes
a strip of each frame and sweeps the picture over the GOP, so every frame is
about the same size and there are no periodic IDRs at all; x264, NVENC and the
Jetson encoders offer it, and it is what Voysys runs. **A long GOP with IDRs
on demand** keeps the burst but makes it rare and purposeful; every ONVIF camera
offers a GOP setting (`GovLength`) and most a keyframe request. The Hikvision
demo camera has no intra refresh in either codec — its capability document
was read to be sure — so it takes the second route.

## Decision

1. **The publisher is asked for recovery points, not a keyframe cadence.**
   `recovery-request` (ADR-free since the ladder, `docs/protocol/seydd.md`) is
   the mechanism by which a pilot gets a decodable picture. The periodic
   keyframe is a safety net for a publisher that ignores requests.
2. **`maxGopMs` is that safety net, and it is long.** `latency` and `balanced`
   ask for 10 000 ms, `quality` for 4 000 ms — shorter because that profile
   freezes on loss until an IDR (`on_loss: freeze-until-idr`), so a publisher
   that ignores requests would otherwise freeze it for ten seconds. The field
   means "the longest you may go without a full recovery point", where an IDR
   and a completed intra-refresh sweep both count.
3. **`video-config` gains `preferIntraRefresh: true`** on every profile. A
   publisher that can refresh gradually should, with any sweep period up to
   `maxGopMs` (shorter repairs a loss sooner at no bitrate cost — the sim
   sweeps in one second); one that cannot uses the longest GOP its encoder
   allows up to `maxGopMs` and answers `recovery-request` with an IDR.
   Additive on the wire; a publisher that ignores it behaves as before.
4. **Publishers in the repo follow it.** The sim and the Python SDK example
   encode with `intra-refresh=1`, sweeping once per second and forcing an IDR
   every `maxGopMs` because FFmpeg cannot answer a request. The demo bridge
   applies `maxGopMs` to the camera's `GovLength` exactly as it applies
   `maxBitrateKbps` to `vbrUpperCap`, and keeps answering requests with
   `requestKeyFrame`. `docs/encoder-setup.md` says how to do the same on
   other encoders and cameras.

## Consequences

- **A joining pilot depends on the request path.** With a 10 s GOP and no one
  answering `recovery-request`, the first picture takes up to 10 s. This is
  the trade made explicit: the publisher-control channel is now load-bearing
  for joins, not just for adaptation. Measured on the camera with the bridge:
  first picture in about 150 ms. The sim, which cannot answer, waits for its
  forced IDR; `tools/seyd-smoke.py` waits for the first decoded frame before
  it samples, for that reason.
- **Loss recovery is one round trip plus the publisher's reaction, not "at
  most one GOP".** On the camera that is ~150 ms; on a publisher answering
  with intra refresh it is gradual over one sweep. A publisher that ignores
  requests now shows a broken picture for up to `maxGopMs`, where it showed one
  for up to a second before — which is why the request path must be verified
  when a camera is brought up, and `tools/keyframe-probe.py` exists to do it.
- **The presentation delay can be re-measured.** ADR 0005's 50/100/150 ms
  budgets were sized against a once-per-second burst that no longer happens.
  On the sim with intra refresh, decode-on-arrival paints with 4.5 ms mean
  judder; on the camera at GOP 250, 0.9 ms. Re-running ADR 0005's table with
  smaller budgets is the next step, not part of this decision.
- **`quality` keeps a shorter net** and therefore still has a burst every 4 s.
  That profile spends 150 ms of presentation delay and can afford it.
- **Camera provisioning changes.** The demo camera's documented GOP of 25
  becomes "from the profile"; `bridge.py` writes it on every `video-config`,
  so a camera reset to factory GOP heals itself at the next demo start.

## Alternatives rejected

- **Keep 1 s IDRs and hide them with the presentation delay.** That is the
  status quo; it works, at the cost of 100 ms of latency the operator pays on
  every frame to absorb one frame a second.
- **Intra refresh only.** Not available on the demo camera nor, in the survey,
  on any ONVIF camera; a policy the reference hardware cannot follow is not a
  policy.
- **Make `maxGopMs` unlimited.** Removes the safety net entirely; a publisher
  that ignores requests would then stay broken until reconnect.
