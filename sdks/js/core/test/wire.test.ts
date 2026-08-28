import { describe, expect, it } from 'vitest';
import { encodeHeader, parseChunk, seqDelta, tsDeltaUs, encodeMessage, HEADER_LEN, FEC_REED_SOLOMON, FLAG2_END_OF_FRAME, FLAG2_FRAME_META, encodeFrameMeta, parseFrameMeta } from '../src/wire.js';

describe('wire v2', () => {
  const h = { keyframe: true, fecType: FEC_REED_SOLOMON, channelId: 3, frameId: 0xbeef, chunkIdx: 9, n: 8, k: 4,
    flags2: FLAG2_FRAME_META | FLAG2_END_OF_FRAME, lastLen: 517, chunkLen: 1200, sendTs: 0xdeadbeef, blockIdx: 2 };
  it('round-trips and matches the Rust byte layout', () => {
    const bytes = encodeHeader(h);
    expect(bytes[0]).toBe(0x80 | (2 << 4) | 2);
    expect(bytes[1]).toBe(3);
    expect([...bytes.subarray(2, 4)]).toEqual([0xbe, 0xef]);
    expect([...bytes.subarray(14, 18)]).toEqual([0xde, 0xad, 0xbe, 0xef]);
    expect([...bytes.subarray(18, 20)]).toEqual([0, 2]);
    const full = new Uint8Array(HEADER_LEN + 3); full.set(bytes); full.set([1, 2, 3], HEADER_LEN);
    const p = parseChunk(full)!;
    expect(p.header).toEqual(h);
    expect([...p.payload]).toEqual([1, 2, 3]);
  });
  it('rejects bad input', () => {
    const b = encodeHeader(h);
    expect(parseChunk(b.subarray(0, 19))).toBeNull();
    const v1 = b.slice(); v1[0] = (v1[0] & 0xf0) | 1; expect(parseChunk(v1)).toBeNull();
    const bad = b.slice(); bad[0] = (bad[0] & 0x8f) | (5 << 4); expect(parseChunk(bad)).toBeNull();
    const idx = b.slice(); idx[4] = 0; idx[5] = 12; expect(parseChunk(idx)).toBeNull();
  });
  it('wrap-aware deltas', () => {
    expect(seqDelta(2, 65535)).toBe(3); expect(seqDelta(65535, 2)).toBe(-3);
    expect(tsDeltaUs(10, 0xffffffff - 5)).toBe(16); expect(tsDeltaUs(5, 10)).toBe(-5);
  });
  it('frame meta', () => {
    const m = { captureTsUs: 1700000000123456n, seqInGop: 7 };
    const buf = new Uint8Array(11); buf.set(encodeFrameMeta(m)); buf[10] = 0x42;
    const p = parseFrameMeta(buf)!;
    expect(p.meta).toEqual(m); expect([...p.rest]).toEqual([0x42]);
  });
  it('single-chunk message', () => {
    const d = encodeMessage(2, 7, new Uint8Array([9, 9]), 123);
    const p = parseChunk(d)!;
    expect(p.header.n).toBe(1); expect(p.header.k).toBe(0); expect(p.header.frameId).toBe(7);
    expect(p.header.flags2 & FLAG2_END_OF_FRAME).toBeTruthy(); expect([...p.payload]).toEqual([9, 9]);
  });
});
