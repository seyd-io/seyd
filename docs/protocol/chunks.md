# Datagram framing: chunks, blocks, frames, channels

Header layout is ADR 0001 (`packages/seyd-wire/src/v2.rs`). This document is
the semantics on top of it.

## Channels

`channel_id` 0 is reserved (control stream only, never appears in datagrams).
Application channels are 1..255, declared by the agent in `welcome` and
`announce` as `{id, kind, name, codec, fps}`. Kinds:

| kind | direction | carrier | notes |
|---|---|---|---|
| `video` | agent → pilot | datagrams, FEC blocks | payload = H.264 Annex B NAL units |
| `sensor` | agent → pilot | datagrams, `n=1,k=0` per message | one message per datagram, `frame_id` = per-channel sequence |
| `command` | pilot → agent | datagrams, `n=1,k=0` per message | same framing; pilot fills `send_ts` |

Reliable stream-carried kinds are defined in PLAN.md and are not part of this
milestone.

## Video frames and FEC blocks

A video frame (one access unit, or the NAL units of one picture) is sent as one
or more **blocks**. A block is `n` data chunks (n ≤ `BLOCK_DATA_CHUNKS` = 8)
plus `k` parity chunks computed over those `n` chunks only (`seyd-fec`,
Cauchy, chunks zero-padded to `chunk_len`; `last_len` carries the true length
of data chunk `n-1`). `k = parity_count(n, pct, 16)` where `pct` is the
profile's delta or key percentage.

* `block_idx` counts blocks within the frame from 0.
* `FLAG2_END_OF_FRAME` is set on **every chunk of the last block**.
* `FLAG2_FRAME_META` is set on block 0 / chunk 0 whose payload then starts with
  the 10-byte `FrameMeta`.
* `keyframe` is set on every chunk of a keyframe.
* `chunk_len` is fixed within a block; 1000 until PMTUD raises it.
* All chunks of a frame carry the same `frame_id` (per channel, wraps at 2^16).

The sender emits a block as soon as it has `n` chunks of payload (or the frame
ends), so a large keyframe starts leaving before the encoder has finished it.

## Receiver rules (both TS and Rust receivers)

* Frame-id comparisons use the signed 16-bit delta (`seyd_wire::seq_delta`).
* A chunk older than the newest seen frame by more than `MAX_REORDER` = 4 is
  dropped. A chunk for a frame at or before the last decoded frame is dropped
  and counted as too-late.
* Recovery is eager: a block is complete as soon as `data_rx + parity_rx ≥ n`.
* A frame is decodable when every block 0..last is complete and the last block
  had `END_OF_FRAME`. Frames are handed to the decoder in strictly increasing
  `frame_id` order; anything older is closed out as lost.
* A frame's close-out timer (profile `deadline_delta` / `deadline_key` ms) is
  re-armed on every chunk — it measures silence, not elapsed time.
* On close-out the receiver sends `loss {ch, frame_id, key}` on the control
  stream (see `control-stream.md`) and marks the picture degraded until the
  next clean keyframe.

## Sensor and command messages

One message per datagram: header with `n=1, k=0, last_len = payload length,
chunk_len = payload length, block_idx = 0, END_OF_FRAME set`. Receivers treat
`frame_id` as a sequence number and may drop stale (older than newest) messages
for `latest-wins` channels. Payload encoding is the channel's `codec`
(`json` for this milestone).
