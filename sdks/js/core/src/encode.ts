// Copyright 2026 Anton Gravestam
// SPDX-License-Identifier: Apache-2.0
// Sender-side framing: split a frame into FEC blocks (docs/protocol/chunks.md).
// The pilot only sends single-chunk messages, but the block encoder is the
// exact mirror of the receiver and is what the reassembler tests drive.
import { encodeParity, parityCount } from './fec.js';
import { encodeHeader, encodeFrameMeta, FEC_NONE, FEC_REED_SOLOMON, FLAG2_END_OF_FRAME, FLAG2_FRAME_META, FrameMeta, HEADER_LEN } from './wire.js';

export const BLOCK_DATA_CHUNKS = 8;

export interface EncodeFrameOptions {
  channelId: number;
  frameId: number;
  keyframe: boolean;
  fecPct: number;
  chunkLen?: number;
  blockDataChunks?: number;
  sendTs?: number;
  meta?: FrameMeta;
  discardable?: boolean;
}

/** Split one frame into wire chunks (data then parity, block by block). */
export function encodeFrame(payload: Uint8Array, o: EncodeFrameOptions): Uint8Array[] {
  const chunkLen = o.chunkLen ?? 1000;
  const perBlock = o.blockDataChunks ?? BLOCK_DATA_CHUNKS;
  let body = payload;
  if (o.meta) {
    const m = encodeFrameMeta(o.meta);
    body = new Uint8Array(m.length + payload.length);
    body.set(m); body.set(payload, m.length);
  }
  const totalChunks = Math.max(1, Math.ceil(body.length / chunkLen));
  const totalBlocks = Math.ceil(totalChunks / perBlock);
  const out: Uint8Array[] = [];
  for (let b = 0; b < totalBlocks; b++) {
    const first = b * perBlock;
    const n = Math.min(perBlock, totalChunks - first);
    const slices: Uint8Array[] = [];
    for (let i = 0; i < n; i++) {
      const start = (first + i) * chunkLen;
      slices.push(body.subarray(start, Math.min(start + chunkLen, body.length)));
    }
    const lastLen = slices[n - 1].length;
    const k = parityCount(n, o.fecPct, 16);
    let parity: Uint8Array[] = [];
    if (k) {
      const padded = slices.map((s) => { if (s.length === chunkLen) return s; const p = new Uint8Array(chunkLen); p.set(s); return p; });
      parity = encodeParity(padded, k);
    }
    const last = b === totalBlocks - 1;
    let flags2 = last ? FLAG2_END_OF_FRAME : 0;
    if (o.discardable) flags2 |= 1 << 1;
    if (b === 0 && o.meta) flags2 |= FLAG2_FRAME_META;
    const all = slices.concat(parity);
    for (let idx = 0; idx < all.length; idx++) {
      const chunk = new Uint8Array(HEADER_LEN + all[idx].length);
      encodeHeader({
        keyframe: o.keyframe, fecType: k ? FEC_REED_SOLOMON : FEC_NONE, channelId: o.channelId,
        frameId: o.frameId, chunkIdx: idx, n, k,
        // FRAME_META flag only on block 0 chunk 0; END_OF_FRAME on every chunk of the last block.
        flags2: (idx === 0 || !(flags2 & FLAG2_FRAME_META)) ? flags2 : (flags2 & ~FLAG2_FRAME_META),
        lastLen, chunkLen, sendTs: (o.sendTs ?? 0) >>> 0, blockIdx: b,
      }, chunk);
      chunk.set(all[idx], HEADER_LEN);
      out.push(chunk);
    }
  }
  return out;
}
