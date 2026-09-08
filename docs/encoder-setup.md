# Configuring a video publisher for Seyd

Seyd relays encoded video without touching it, so most of the latency a pilot
sees is decided before Seyd receives a byte — by the encoder. This page is
what to set, why, and how to check it, for the encoders and cameras we have
met so far. It is the publisher's half of the contract in
`docs/protocol/seydd.md`: Seyd states targets in `video-config` and asks for
recovery points with `recovery-request`; the publisher decides how to meet
them. `docs/latency-sources.md` has the measurements behind every rule.

## The rules

| Rule | Why | Cost of getting it wrong |
|---|---|---|
| **No B-frames** (H.264 Baseline, or Main/High with `bframes=0`) | A B-frame is emitted after the frame that follows it; the decoder must hold a frame to reorder | One frame interval per B-frame, before Seyd sees anything |
| **No frame threading / lookahead** in the encoder | Same: output waits for later input | One or more frame intervals |
| **Parameter sets inline on every keyframe** (SPS/PPS in the access unit) | A pilot joins mid-stream and needs them with the first IDR | Blank picture until the next keyframe that carries them |
| **Keyframes on demand, not on a timer** (ADR 0009) | A periodic IDR is 8–25× a delta frame and lands late; measured as the p95 tail and as a once-per-GOP hitch. Seyd asks for one whenever a pilot needs it | 100 ms of presentation delay spent hiding the hitch; ~40 % of bitrate on a static scene |
| **Periodic intra refresh where the encoder has it**, otherwise **the longest GOP `maxGopMs` allows** | Intra refresh removes the burst entirely; a long GOP makes it rare | See above |
| **Answer `recovery-request` with an IDR** (or an intra-refresh cycle / LTR frame if you can) | It is the only way a joining or loss-hit pilot gets a picture once IDRs are rare | Up to `maxGopMs` of blank or broken picture |
| **Bitrate cap at or under `maxBitrateKbps`**, VBV about 100 ms | The link carries the cap plus Seyd's FEC; a long VBV lets a motion burst queue for its whole length | Queue latency, or loss |
| **Full frame rate; never trade cadence for pixels** | For a remote pilot the frame interval is latency | 25 → 15 fps is +27 ms on every reaction |

## Encoders

### FFmpeg / x264 (the sim, the Python SDK example)

```
-c:v libx264 -tune zerolatency -preset veryfast -profile:v baseline -bf 0 \
-g 30 -keyint_min 30 -x264-params scenecut=0:intra-refresh=1 \
-force_key_frames "expr:gte(t,n_forced*10)" \
-b:v 3000k -maxrate 3000k -bufsize 300k
```

`intra-refresh=1` replaces periodic IDRs with a sweep whose period is `-g`
(one second here: a loss is fully repaired within a second at no bitrate cost).
`zerolatency` also disables lookahead and frame threading and writes the VUI
fields that tell a decoder there is no reordering. FFmpeg cannot emit an IDR on
request, so `-force_key_frames` supplies the safety net at `maxGopMs`; x264
honours it in intra-refresh mode (verified with `ffprobe`: IDRs exactly at the
forced instants). `scenecut=0` keeps IDR timing predictable. A publisher
built on libx264 directly can answer `recovery-request` by setting
`i_type = X264_TYPE_IDR` on the next picture, and then needs no forced cadence.

### GStreamer

`x264enc tune=zerolatency speed-preset=veryfast bframes=0 intra-refresh=true
key-int-max=30 bitrate=3000` with `video/x-h264,profile=baseline`. Answer a
`recovery-request` by sending a `GstForceKeyUnit` event upstream
(`gst_video_event_new_upstream_force_key_unit`). For hardware encoders
(`nvv4l2h264enc` on Jetson, `v4l2h264enc` on Pi) see below.

### NVIDIA NVENC (desktop GPUs, Jetson)

`NV_ENC_CONFIG_H264.enableIntraRefresh = 1`, `intraRefreshPeriod` = frames per
sweep, `intraRefreshCnt` = frames the sweep spans; `gopLength =
NVENC_INFINITE_GOPLENGTH` and `frameIntervalP = 1` (no B-frames);
`NV_ENC_PIC_PARAMS.encodePicFlags = NV_ENC_PIC_FLAG_FORCEIDR` to answer a
request. Also supports LTR (`enableLTR`), which is the cheapest rung of Seyd's
recovery ladder. On Jetson through V4L2: `V4L2_CID_MPEG_VIDEOENC_ENABLE_INTRA_REFRESH`
/ `INTRA_REFRESH_FRAME_INTERVAL`, `V4L2_CID_MPEG_VIDEOENC_FORCE_IDR_FRAME`.

### Raspberry Pi 5

Has no hardware H.264 encoder; software x264 as above, with `-threads 1` or
`sliced-threads` rather than frame threads. The stock defaults ship B-frames
and frame threading, so an unconfigured Pi adds two frames before Seyd starts.

## IP cameras

No ONVIF camera in the survey exposes intra refresh, so cameras take the long
GOP route. Every ONVIF Profile S/T camera exposes the GOP as `GovLength` in
its `VideoEncoderConfiguration` (Media/Media2); the keyframe request is
vendor-specific.

### Hikvision (the demo camera, `examples/demo-robot/`)

- GOP: `PUT /ISAPI/Streaming/channels/<ch>` with `<GovLength>` (frames) and
  `<keyFrameInterval>` (ms) patched together — this firmware keeps whichever
  was written last. Range from `/capabilities` (1–400 on the DS-2DE2A404IWG1-E).
  Takes effect on the running stream. `hikvision.py` `set_gop()`; `bridge.py`
  applies `maxGopMs` from every `video-config`.
- IDR on demand: `PUT /ISAPI/Streaming/channels/<ch>/requestKeyFrame`.
  Measured 2026-09-08: the IDR arrives 96–155 ms after the request (35–40 ms of
  which is the HTTP call), at GOP 25 and at GOP 250 alike, and it restarts the
  GOP phase.
- Profile: **Baseline** (`H264Profile`), because Main/High emit B-frames on
  this firmware.
- Do not enable `SmartCodec` (H.264+/H.265+): dynamic GOP with unpredictable
  keyframe timing.

### Axis

`VAPIX param.cgi` `Image.I0.Stream.GOP` (or per-profile in the stream
settings); Zipstream ships **dynamic GOP on by default** and should be set to
a fixed GOP for Seyd. Keyframe request: not exposed over VAPIX in the versions
surveyed — verify with the probe below before relying on a long GOP.

### Generic ONVIF

`SetVideoEncoderConfiguration` with `H264.GovLength` (Media) or `GovLength`
(Media2). There is no standard keyframe request, so unless the vendor offers
one, keep `GovLength` at the profile's `maxGopMs` only if a broken picture for
up to that long on unrecoverable loss is acceptable; otherwise choose a shorter
GOP and accept the hitch.

## Verify it

```
tools/keyframe-probe.py --input "rtsp://user:pass@<ip>:554/<path>" --seconds 30 \
    --request-at 12,20 --request-cmd '<the vendor keyframe request>'
```

Prints every keyframe with its arrival time and the gap since the last, the
periodic interval, and for each request how long the publisher took to answer.
What to expect from a well-configured publisher: a periodic gap of `maxGopMs`
or none at all, a request answered within a few frames, and — with
`ffprobe -show_streams` — `has_b_frames=0`.

Then look at the pilot's side with `tools/latency-ab.py --pd 0`: arrival gaps
over two frame intervals should be zero between keyframes, and paint judder a
few milliseconds. Through `tools/link-shaper.py` at the customer's uplink rate
is where a keyframe burst shows itself; on a LAN it costs nothing.
