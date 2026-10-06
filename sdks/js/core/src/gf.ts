// Copyright 2026 Anton Gravestam
// SPDX-License-Identifier: Apache-2.0
// GF(256) arithmetic, field polynomial 0x11d. Part of the wire contract —
// mirrors packages/seyd-fec/src/gf.rs and the legacy fec.py/fec.js exactly.
export const POLY = 0x11d;

const EXP = new Uint8Array(512);
const LOG = new Uint8Array(256);
{
  let x = 1;
  for (let i = 0; i < 255; i++) {
    EXP[i] = x;
    LOG[x] = i;
    x <<= 1;
    if (x & 0x100) x ^= POLY;
  }
  for (let i = 255; i < 512; i++) EXP[i] = EXP[i - 255];
}

export const mul = (a: number, b: number): number => (a === 0 || b === 0 ? 0 : EXP[LOG[a] + LOG[b]]);
export const inv = (a: number): number => EXP[255 - LOG[a]];

// Flat 64 KiB multiply table: MT[(c << 8) | b] === c * b. One contiguous
// Uint8Array indexed by a precomputed base is markedly faster in JS engines
// than nested tables.
const MT = new Uint8Array(65536);
for (let a = 0; a < 256; a++) for (let b = 0; b < 256; b++) MT[(a << 8) | b] = mul(a, b);

/** dst ^= coeff * src, bytewise. */
export function maddInto(dst: Uint8Array, src: Uint8Array, coeff: number): void {
  if (coeff === 0) return;
  const base = coeff << 8;
  const n = dst.length;
  for (let j = 0; j < n; j++) dst[j] ^= MT[base | src[j]];
}
