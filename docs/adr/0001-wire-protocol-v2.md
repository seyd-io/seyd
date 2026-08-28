# ADR 0001 — Wire protocol v2

**Status:** accepted (2026-08-28)

## Context

The prototype's chunk header (`packages/agent/fec.py`, `packages/pilot/fec.js`)
is 10 bytes: flags/version, `frame_id`, `chunk_idx`, `n`, `k`, `last_len`. It
carries one video channel, FEC is computed per whole frame, and there is no
timestamp. Three product requirements break it:

1. Multiple streams per connection (several cameras, sensor and command
   channels) — there is no channel id.
2. Sub-frame pipelining — parity per whole frame means the first byte of a
   keyframe cannot leave until the last byte has been read from the encoder.
3. Latency measurement — nothing on the wire says when a chunk was sent.

## Decision

A 20-byte header, version nibble `2`. Every v1 field keeps its meaning; the
Reed-Solomon construction (GF(256), poly `0x11d`, Cauchy
`A[i][j] = 1/(i XOR (k+j))`) is unchanged and remains part of the wire contract.

```
byte  0      bit 7 keyframe | bits 4-6 fec_type (0 none, 2 RS) | bits 0-3 version = 2
byte  1      channel_id   u8      0 reserved for control; 1..255 application channels
bytes 2-3    frame_id     u16 BE  per channel, wraps; compare with signed 16-bit delta
bytes 4-5    chunk_idx    u16 BE  0..n-1 data, n..n+k-1 parity, within the block
bytes 6-7    n            u16 BE  data chunks in this FEC block
byte  8      k            u8      parity chunks in this FEC block
byte  9      flags2       bit 0 frame_meta present (block 0, chunk 0)
                          bit 1 discardable (may be dropped under pressure)
                          bit 2 end_of_frame (this is the last block of the frame)
bytes 10-11  last_len     u16 BE  real length of data chunk n-1 of this block
bytes 12-13  chunk_len    u16 BE  payload size of full chunks in this block
bytes 14-17  send_ts      u32 BE  low 32 bits of sender monotonic microseconds
bytes 18-19  block_idx    u16 BE  index of this FEC block within the frame
bytes 20+    payload
```

* `n`/`k` describe a **block**, not the frame. A frame is one or more blocks;
  the receiver assembles blocks in `block_idx` order until `end_of_frame`.
  Block size is a sender policy (default 8 data chunks) and is not in the
  header — the receiver only needs `n`, `k`, `block_idx`, `end_of_frame`.
* `chunk_len` is present because DPLPMTUD may change the chunk size between
  frames. It is fixed within a block. A receiver that has only parity chunks
  or the short last chunk of a block still knows the padding length.
* `send_ts` wraps every ~71 minutes; receivers use wrap-aware deltas. It gives
  intra-frame spread and one-way delay once the control channel's `ping/pong`
  has estimated the clock offset.
* Frame meta, when `flags2 bit0` is set, is a fixed 10-byte prefix of the
  payload of block 0 / chunk 0: `capture_ts_us u64 BE`, `seq_in_gop u16 BE`.
  It is kept out of the header because most channels never need it.
* Unreliable sensor/command channels use the same header with `n=1, k=0`
  (or FEC if configured); their `frame_id` is a per-channel sequence.
* The version nibble makes v1 → v2 a hard cutover. A receiver counts and
  drops unknown versions; it never guesses.

## Consequences

* `tools/fec-vectors.py` stays the generator for the FEC construction test,
  and gains v2 header cases; the Rust (`seyd-wire`, `seyd-fec`) and TS
  (`@seyd/core`) implementations must pass byte-for-byte.
* Header overhead rises from 10 to 20 bytes per chunk (~1.5% at 1350-byte
  chunks). Accepted; PMTUD more than recovers it.
* Interleaving parity across blocks (burst protection) is representable —
  a block's parity may reference the previous block — but is not enabled in
  v2.0; see PLAN.md §1.7.
