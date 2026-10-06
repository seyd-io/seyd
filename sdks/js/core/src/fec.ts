// Copyright 2026 Anton Gravestam
// SPDX-License-Identifier: Apache-2.0
// Reed-Solomon erasure coding over GF(256), Cauchy generator matrix.
// Construction is the wire contract: A[i][j] = 1 / (i XOR (k + j)), n + k <= 256.
import { inv, mul, maddInto } from './gf.js';

const matrixCache = new Map<number, Uint8Array[]>();

export function cauchyMatrix(n: number, k: number): Uint8Array[] {
  if (n + k > 256) throw new RangeError(`n+k must be <= 256, got n=${n} k=${k}`);
  const key = n * 1024 + k;
  let m = matrixCache.get(key);
  if (m) return m;
  m = [];
  for (let i = 0; i < k; i++) {
    const row = new Uint8Array(n);
    for (let j = 0; j < n; j++) row[j] = inv(i ^ (k + j));
    m.push(row);
  }
  matrixCache.set(key, m);
  return m;
}

function roundHalfEven(x: number): number {
  const f = Math.floor(x);
  const d = x - f;
  if (d > 0.5) return f + 1;
  if (d < 0.5) return f;
  return f % 2 === 0 ? f : f + 1;
}

/** Parity chunks for an n-chunk block at pct overhead (matches seyd-fec). */
export function parityCount(n: number, pct: number, cap = 16): number {
  if (pct <= 0 || n <= 0) return 0;
  return Math.max(1, Math.min(cap, n, roundHalfEven((n * pct) / 100)));
}

/** k parity chunks over n equal-length data chunks. */
export function encodeParity(chunks: Uint8Array[], k: number): Uint8Array[] {
  if (k <= 0 || chunks.length === 0) return [];
  const size = chunks[0].length;
  const matrix = cauchyMatrix(chunks.length, k);
  return matrix.map((row) => {
    const acc = new Uint8Array(size);
    for (let j = 0; j < chunks.length; j++) maddInto(acc, chunks[j], row[j]);
    return acc;
  });
}

function invertMatrix(rows: Uint8Array[]): Uint8Array[] | null {
  const m = rows.length;
  const aug = rows.map((row, i) => {
    const r = new Uint8Array(2 * m);
    r.set(row, 0);
    r[m + i] = 1;
    return r;
  });
  for (let col = 0; col < m; col++) {
    let pivot = -1;
    for (let r = col; r < m; r++) if (aug[r][col]) { pivot = r; break; }
    if (pivot < 0) return null;
    if (pivot !== col) { const t = aug[col]; aug[col] = aug[pivot]; aug[pivot] = t; }
    const scale = inv(aug[col][col]);
    for (let j = 0; j < 2 * m; j++) aug[col][j] = mul(aug[col][j], scale);
    for (let r = 0; r < m; r++) {
      if (r === col || !aug[r][col]) continue;
      const f = aug[r][col];
      for (let j = 0; j < 2 * m; j++) aug[r][j] ^= mul(f, aug[col][j]);
    }
  }
  return aug.map((r) => r.subarray(m));
}

/**
 * Reconstruct missing data chunks. `data` has n entries (null = erased),
 * `parity` has k. Returns the full data array or null if unrecoverable.
 */
export function decode(data: (Uint8Array | null)[], parity: (Uint8Array | null)[]): Uint8Array[] | null {
  const n = data.length, k = parity.length;
  const lost: number[] = [];
  for (let i = 0; i < n; i++) if (!data[i]) lost.push(i);
  if (lost.length === 0) return data as Uint8Array[];

  const have: number[] = [];
  for (let p = 0; p < k; p++) if (parity[p]) have.push(p);
  if (have.length < lost.length) return null;
  const use = have.slice(0, lost.length);

  let size = 0;
  for (const c of data) if (c) { size = c.length; break; }
  if (!size) for (const c of parity) if (c) { size = c.length; break; }
  if (!size) return null;

  const matrix = cauchyMatrix(n, k);
  const syndromes = use.map((p) => {
    const acc = new Uint8Array(size);
    acc.set(parity[p]!);
    for (let j = 0; j < n; j++) if (data[j]) maddInto(acc, data[j]!, matrix[p][j]);
    return acc;
  });
  const sub = use.map((p) => {
    const row = new Uint8Array(lost.length);
    for (let c = 0; c < lost.length; c++) row[c] = matrix[p][lost[c]];
    return row;
  });
  const inverse = invertMatrix(sub);
  if (!inverse) return null;

  const out = data.slice() as Uint8Array[];
  for (let r = 0; r < lost.length; r++) {
    const acc = new Uint8Array(size);
    for (let c = 0; c < lost.length; c++) maddInto(acc, syndromes[c], inverse[r][c]);
    out[lost[r]] = acc;
  }
  return out;
}
