# The publisher contract

Seyd relays encoded video without touching it, so most of the latency a
pilot sees is decided before Seyd receives a byte. The publisher (the user's
encoder or camera) owns resolution, preset, VBV and the actual GOP. Seyd owns
what the transport can observe: a bitrate ceiling, a latency budget, a GOP
bound, and when a pilot needs a recovery point. **Seyd states intent; the
publisher decides how to meet it.** The plan must say how each message below
is answered, and "ignored, because the camera cannot" is an acceptable
answer only with its cost stated.

## How Seyd asks

Daemon: JSON datagrams on `[publisher_control]`. Python:
`on_requested_config(config)`, `on_recovery_request(channel, kind, reason)`.
C: the same plus `on_layer_changed`. Rust: `AgentEvent::RequestedConfig`,
`RecoveryRequest`, `LayerChanged`. The content is identical everywhere.

## `video-config`: the targets

```json
{"type": "video-config", "channel": 1, "profile": "balanced",
 "maxBitrateKbps": 3000, "latencyBudgetMs": 100, "maxGopMs": 10000,
 "preferIntraRefresh": true, "suggestedFps": 0, "reason": "profile"}
```

| Field | What it asks |
|---|---|
| `maxBitrateKbps` | The current request: never above the profile's ceiling, never below 25 % of it (or of the channel's `max_bitrate_kbps` when that is lower). The link carries this plus Seyd's FEC. A cap at or under it and a VBV of about 100 ms keeps a motion burst from queueing. |
| `latencyBudgetMs` | End-to-end delay the profile is prepared to spend; size encoder buffers from it. |
| `maxGopMs` | The longest the publisher may go without a full recovery point (an IDR or a completed intra-refresh sweep). A ceiling and a safety net, never a cadence: 10 s on `latency` and `balanced`, 4 s on `quality`. |
| `preferIntraRefresh` | Refresh gradually where the encoder can (x264, NVENC, Jetson), any sweep period up to `maxGopMs`. A publisher that cannot uses the longest GOP up to `maxGopMs` and answers requests with an IDR. |
| `suggestedFps` | Non-zero only when the request is pinned at the floor and the link is still congested; the last rung. 0 means restore full cadence. |
| `reason` | `profile` once at start, `pilot-request` on a profile change, `abr-down` / `abr-up` as the controller moves inside the ceiling. |

The profiles' numbers are in `qos-profiles.md`. There is no resolution in a
profile and there must never be one in Seyd configuration.

**Degrade resolution first, frame rate last.** 25 to 15 fps stretches the
frame interval from 40 to 67 ms, so the operator waits up to 27 ms longer to
see the result of their own input. When `maxBitrateKbps` falls, spend fewer
bits on pixels and keep the cadence; where the publisher already encodes the
picture twice, let Seyd select the stream (simulcast) instead.

## `recovery-request`: keyframes on demand

```json
{"type": "recovery-request", "channel": 1, "kind": "idr", "reason": "pilot-loss"}
```

Sent when a pilot needs a recovery point: on join, on a loss FEC could not
repair, on a backlog drop. Never emit an IDR every second on a timer: on the
demo camera's static scene periodic IDRs were 43 % of the stream, and through
a shaped link they were the whole p95 tail.

`kind`, cheapest first: `ltr` (a frame predicted from a long-term reference
the decoder still holds), `intra_refresh` (one gradual sweep, no burst),
`idr` (a full keyframe: 30 to 60 KB against a 2 KB delta; on a 2 Mbps uplink
120 to 240 ms of serialisation). A publisher that cannot honour a kind
answers with what it can; one that ignores `kind` and always sends an IDR is
fine. When the pilot's decoder itself needs a keyframe, `kind` is `idr`.

The ladder starts at `ltr` and climbs only after the profile's
`recovery_grace_ms` with loss continuing; it resets after two quiet seconds.
`recovery_ladder = false` forces `idr` for every request, for a publisher
that mishandles the other kinds. Rate limit: one request per 250 ms per
channel. After a backlog drop Seyd withholds deltas until a recovery point
or `recovery_grace_ms` (700 / 900 / 1400 ms by profile), so a publisher that
never produces one latches the stream silent for that long each time.

## `layer`

```json
{"type": "layer", "channel": 1, "layer": 0, "name": "low", "reason": "down"}
```

The forwarded simulcast layer changed. Seyd switches on that layer's next
keyframe regardless; forcing an IDR on the named stream makes the switch
cost one frame instead of up to a GOP. Nothing is required of a publisher
that ignores it.

## What ignoring all this costs

The robot keeps streaming with whatever it was configured with, and loses:
rate control (a congested link tears frames instead of thinning them), first
picture (a joining pilot waits up to `maxGopMs`; with the demo camera's
bridge answering, first picture is about 150 ms), and loss recovery (a
broken picture until the next recovery point, where an answered request
costs one round trip plus the publisher's reaction).

## The rules, in one place

- No B-frames, no frame threading or lookahead: each holds a frame back
  before Seyd sees it.
- Parameter sets (SPS/PPS) inline on every keyframe, because a pilot joins
  mid-stream.
- Keyframes on demand: intra refresh where available, otherwise the longest
  GOP `maxGopMs` allows, and always answer `recovery-request`.
- Bitrate cap at or under `maxBitrateKbps`, VBV about 100 ms.
- Full frame rate; degrade resolution first and cadence last.
- Baseline or Constrained Baseline H.264 is the safe profile (Main/High
  emit B-frames on some camera firmware); the codec string's level must
  admit the real picture (`avc1.42001f` = level 3.1 = up to 1280x720;
  `avc1.420028` = level 4.0 for 1080p).

## Encoders (from `docs/encoder-setup.md`; read it for the full recipes)

- **FFmpeg / x264** (pipe into an SDK, or publish RTP to the daemon):
  `-c:v libx264 -tune zerolatency -preset veryfast -profile:v baseline -bf 0
  -g 30 -keyint_min 30 -x264-params scenecut=0:intra-refresh=1
  -force_key_frames "expr:gte(t,n_forced*10)" -b:v 3000k -maxrate 3000k
  -bufsize 300k`. FFmpeg cannot emit an IDR on request, so
  `-force_key_frames` at `maxGopMs` is the safety net. For a pipe into an
  SDK, output Annex B (`-f h264 pipe:1`) with `-bsf:v
  h264_metadata=aud=insert` so access units split at the delimiter without
  a slice parser. A program on libx264 directly answers a request with
  `i_type = X264_TYPE_IDR` on the next picture.
- **GStreamer**: `x264enc tune=zerolatency speed-preset=veryfast bframes=0
  intra-refresh=true key-int-max=30 bitrate=3000` with
  `video/x-h264,profile=baseline`; answer a request with an upstream
  `GstForceKeyUnit` event. `rtph264pay` + `udpsink` publishes RTP to the
  daemon.
- **NVENC / Jetson**: `enableIntraRefresh`, `gopLength =
  NVENC_INFINITE_GOPLENGTH`, `frameIntervalP = 1`, answer with
  `NV_ENC_PIC_FLAG_FORCEIDR`; LTR available (`enableLTR`). Jetson V4L2:
  `V4L2_CID_MPEG_VIDEOENC_ENABLE_INTRA_REFRESH`,
  `V4L2_CID_MPEG_VIDEOENC_FORCE_IDR_FRAME`. These are the driver controls;
  the GStreamer element that wraps them on a Jetson (`nvv4l2h264enc`)
  exposes them as properties whose names and units vary by JetPack release
  and have not been verified by Seyd: read `gst-inspect-1.0 nvv4l2h264enc`
  on the device for the bitrate, VBV, IDR interval, intra-refresh and
  B-frame properties, confirm with `ffprobe` that B-frames are off, and
  confirm at rung 4 of `verification.md` that an upstream force-key-unit
  event produces an IDR within a few frames before relying on it.
- **Raspberry Pi 5**: no hardware H.264; x264 as above with `-threads 1` or
  sliced threads. Stock defaults ship B-frames and frame threading.
- **IP cameras**: no surveyed ONVIF camera exposes intra refresh, so cameras
  take the long-GOP route (`GovLength` in `VideoEncoderConfiguration`) and
  answer requests with a vendor keyframe call. Hikvision:
  `PUT /ISAPI/Streaming/channels/<ch>/requestKeyFrame` (answers in 96 to
  155 ms), Baseline profile, SmartCodec off. Axis: fixed GOP (Zipstream's
  dynamic GOP off); no keyframe request in the surveyed VAPIX versions.
  Generic ONVIF: no standard keyframe request; without a vendor one, a
  shorter GOP is the trade.

## Verify the publisher before promising anything

`tools/keyframe-probe.py --input "rtsp://…" --seconds 30 --request-at 12,20
--request-cmd '<vendor keyframe request>'` prints every keyframe with its
arrival time, the periodic interval, and how long each request took to be
answered. Good: a periodic gap of `maxGopMs` or none, requests answered
within a few frames, and `ffprobe -show_streams` reporting `has_b_frames=0`.
