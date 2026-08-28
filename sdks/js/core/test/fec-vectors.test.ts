// Replays tools/fec-vectors.py (the Python reference) through @seyd/core's FEC,
// exactly as tools/fec-check.js does, via the legacy v1 header.
import { execFileSync } from 'node:child_process';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import { decode } from '../src/fec.js';
import { parseV1, V1_HEADER_LEN, V1_VERSION } from '../src/wire-v1.js';

const hex = (s: string) => { const o = new Uint8Array(s.length / 2); for (let i = 0; i < o.length; i++) o[i] = parseInt(s.substr(i * 2, 2), 16); return o; };

describe('FEC interop with the Python reference', () => {
  const root = resolve(__dirname, '../../../..');
  const raw = execFileSync('python3', [resolve(root, 'tools/fec-vectors.py')], { encoding: 'utf8', maxBuffer: 64 << 20 });
  const vec = JSON.parse(raw);

  it('agrees on the v1 header contract', () => {
    expect(vec.headerLen).toBe(V1_HEADER_LEN);
    expect(vec.version).toBe(V1_VERSION);
  });

  it(`recovers all ${vec.cases.length} vectors byte for byte`, () => {
    let pass = 0;
    for (const c of vec.cases) {
      const erased = new Set<number>(c.erased);
      const data: (Uint8Array | null)[] = new Array(c.n).fill(null);
      const parity: (Uint8Array | null)[] = new Array(c.k).fill(null);
      c.chunks.forEach((h: string, i: number) => {
        if (erased.has(i)) return;
        const p = parseV1(hex(h))!;
        expect(p.n).toBe(c.n); expect(p.k).toBe(c.k); expect(p.lastLen).toBe(c.lastLen);
        expect(p.frameId).toBe(c.frameId); expect(p.isKeyframe).toBe(c.isKeyframe);
        if (p.chunkIdx < c.n) { let b = p.payload; if (b.length < vec.chunkSize) { const q = new Uint8Array(vec.chunkSize); q.set(b); b = q; } data[p.chunkIdx] = b; }
        else parity[p.chunkIdx - c.n] = p.payload;
      });
      const out = decode(data, parity);
      expect(out, c.label).not.toBeNull();
      const total = (c.n - 1) * vec.chunkSize + c.lastLen;
      const rebuilt = new Uint8Array(total);
      let off = 0;
      for (let i = 0; i < c.n; i++) { const take = i === c.n - 1 ? c.lastLen : vec.chunkSize; rebuilt.set(out![i].subarray(0, take), off); off += take; }
      expect(Buffer.from(rebuilt).equals(Buffer.from(hex(c.expected))), c.label).toBe(true);
      pass++;
    }
    console.log(`fec interop (ts): ${pass} passed, 0 failed (seed ${vec.seed})`);
    expect(pass).toBe(vec.cases.length);
  });
});
