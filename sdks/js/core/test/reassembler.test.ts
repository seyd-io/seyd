// Copyright 2026 Anton Gravestam
// SPDX-License-Identifier: Apache-2.0
import { describe, expect, it, vi } from 'vitest';
import { encodeFrame } from '../src/encode.js';
import { AssembledFrame, LossEvent, Reassembler } from '../src/reassembler.js';
import { parseChunk } from '../src/wire.js';

let seed = 1;
const rnd = () => { seed ^= seed << 13; seed ^= seed >>> 17; seed ^= seed << 5; return (seed >>> 0) / 4294967296; };
const payload = (n: number) => { const p = new Uint8Array(n); for (let i = 0; i < n; i++) p[i] = (rnd() * 256) | 0; return p; };

function make() {
  const frames: AssembledFrame[] = []; const losses: LossEvent[] = [];
  const r = new Reassembler({ deadlineDeltaMs: 30, deadlineKeyMs: 60, onFrame: (f) => frames.push(f), onLoss: (l) => losses.push(l) });
  return { r, frames, losses };
}
const feed = (r: Reassembler, chunks: Uint8Array[]) => { for (const c of chunks) { const p = parseChunk(c)!; r.push(p.header, p.payload); } };

describe('Reassembler', () => {
  it('assembles a multi-block keyframe with frame meta', () => {
    const { r, frames } = make();
    const data = payload(20000);
    const chunks = encodeFrame(data, { channelId: 1, frameId: 5, keyframe: true, fecPct: 50, meta: { captureTsUs: 42n, seqInGop: 0 } });
    expect(chunks.length).toBeGreaterThan(20);
    feed(r, chunks);
    expect(frames).toHaveLength(1);
    expect(Buffer.from(frames[0].data).equals(Buffer.from(data))).toBe(true);
    expect(frames[0].captureTsUs).toBe(42n);
    expect(frames[0].keyframe).toBe(true);
    expect(frames[0].recovered).toBe(false);
  });

  it('recovers any k erasures per block', () => {
    const { r, frames } = make();
    const data = payload(12345);
    const chunks = encodeFrame(data, { channelId: 1, frameId: 9, keyframe: false, fecPct: 25 });
    // Drop k chunks of every block.
    const byBlock = new Map<number, Uint8Array[]>();
    for (const c of chunks) { const h = parseChunk(c)!.header; (byBlock.get(h.blockIdx) ?? byBlock.set(h.blockIdx, []).get(h.blockIdx)!).push(c); }
    const kept: Uint8Array[] = [];
    for (const list of byBlock.values()) {
      const k = parseChunk(list[0])!.header.k;
      const idx = new Set<number>(); while (idx.size < k) idx.add((rnd() * list.length) | 0);
      list.forEach((c, i) => { if (!idx.has(i)) kept.push(c); });
    }
    feed(r, kept.reverse());  // reversed: parity first, data later
    expect(frames).toHaveLength(1);
    expect(frames[0].recovered).toBe(true);
    expect(Buffer.from(frames[0].data).equals(Buffer.from(data))).toBe(true);
    expect(r.counters.framesRecovered).toBe(1);
  });

  it('closes out on silence and reports loss; gates decode order', () => {
    vi.useFakeTimers();
    const { r, frames, losses } = make();
    const a = encodeFrame(payload(3000), { channelId: 1, frameId: 1, keyframe: true, fecPct: 0 });
    const b = encodeFrame(payload(3000), { channelId: 1, frameId: 2, keyframe: false, fecPct: 0 });
    feed(r, a.slice(0, 2));      // frame 1 incomplete (3 chunks, no parity)
    feed(r, b);                  // frame 2 complete → frame 1 superseded
    expect(frames.map((f) => f.frameId)).toEqual([2]);
    expect(losses).toHaveLength(1); expect(losses[0].frameId).toBe(1); expect(losses[0].keyframe).toBe(true); expect(losses[0].superseded).toBe(true);
    feed(r, a.slice(2));         // late chunk for an older frame: too late
    expect(r.counters.chunksTooLate).toBe(1);
    const c = encodeFrame(payload(3000), { channelId: 1, frameId: 3, keyframe: false, fecPct: 0 });
    feed(r, c.slice(0, 1));
    vi.advanceTimersByTime(100);
    expect(losses).toHaveLength(2); expect(losses[1].superseded).toBe(false);
    expect(r.counters.keyframesLost).toBe(1);
    vi.useRealTimers();
  });

  it('drops chunks older than MAX_REORDER and handles wraparound', () => {
    const { r, frames } = make();
    const mk = (id: number) => encodeFrame(payload(500), { channelId: 1, frameId: id, keyframe: true, fecPct: 0 });
    feed(r, mk(65534)); feed(r, mk(65535)); feed(r, mk(0)); feed(r, mk(1));
    expect(frames.map((f) => f.frameId)).toEqual([65534, 65535, 0, 1]);
    feed(r, mk(65530));
    expect(r.counters.chunksTooOld).toBe(1);
  });
});

import { describe as d2, it as it2, expect as e2 } from 'vitest';
import { Reassembler as R2 } from '../src/reassembler.js';
import { encodeFrame as enc2 } from '../src/encode.js';

d2('adaptive close-out', () => {
  it2('widens the silence deadline after observing intra-frame jitter', () => {
    let now = 0;
    const frames: number[] = [];
    const losses: number[] = [];
    const r = new R2({ deadlineDeltaMs: 30, deadlineKeyMs: 60, onFrame: (f) => frames.push(f.frameId), onLoss: (l) => losses.push(l.frameId), now: () => now });
    // 20 frames whose two chunks arrive 25 ms apart: legitimate jitter under the 30 ms base.
    for (let id = 0; id < 20; id++) {
      const chunks = enc2(new Uint8Array(1500), { channelId: 1, frameId: id, keyframe: id === 0, fecPct: 0, chunkLen: 1000, sendTs: 0 });
      const a = parseChunk(chunks[0])!, b = parseChunk(chunks[1])!;
      r.push(a.header, a.payload); now += 25; r.push(b.header, b.payload); now += 15;
    }
    e2(frames.length).toBe(20);
    e2(r.counters.deadlineDeltaEffectiveMs).toBeGreaterThanOrEqual(70); // 2 × 25 + 20
    e2(r.counters.deadlineDeltaEffectiveMs).toBeLessThanOrEqual(250);
  });
});
