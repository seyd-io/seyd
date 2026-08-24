// Reed-Solomon erasure decoding over GF(256) + video chunk header parsing.
//
// The receiving half of packages/agent/fec.py. Read that file first — it carries
// the full rationale for the scheme, the wire format, and the measurements. What
// matters here is that every constant below is part of the wire contract and must
// match the agent exactly: the field polynomial, the Cauchy construction, and the
// header layout. A mismatch does not fail loudly; it silently reconstructs wrong
// bytes and shows up only as unexplained decode errors. tools/fec-check.js exists
// to catch precisely that.
//
// Loaded as a plain <script> before pilot.js, so it exports a global. The
// module.exports tail lets the interop harness require() it under node.

(function (root) {
  'use strict';

  const POLY = 0x11d;

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

  const mul = (a, b) => (a === 0 || b === 0) ? 0 : EXP[LOG[a] + LOG[b]];
  const inv = (a) => EXP[255 - LOG[a]];

  // Flat 64 KB multiply table: MT[(c << 8) | b] === c * b in GF(256). One
  // contiguous Uint8Array indexed by a precomputed base is markedly faster in
  // JS engines than a nested array of tables.
  const MT = new Uint8Array(65536);
  for (let a = 0; a < 256; a++) {
    for (let b = 0; b < 256; b++) MT[(a << 8) | b] = mul(a, b);
  }

  // dst ^= coeff * src, bytewise over the whole chunk.
  function maddInto(dst, src, coeff) {
    if (coeff === 0) return;
    const base = coeff << 8;
    for (let j = 0; j < dst.length; j++) dst[j] ^= MT[base | src[j]];
  }

  // ── generator matrix ──────────────────────────────────────────────────────
  // Must reproduce fec.py cauchy_matrix() exactly: A[i][j] = 1 / (i XOR (k+j)).
  const matrixCache = new Map();

  function cauchyMatrix(n, k) {
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

  // Gauss-Jordan inverse of a square GF(256) matrix. Only ever called on a
  // d x d submatrix where d is the erasure count (d <= k <= 16), so the cubic
  // cost is irrelevant.
  function invertMatrix(rows) {
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
      if (pivot < 0) return null;              // impossible for Cauchy
      if (pivot !== col) { const t = aug[col]; aug[col] = aug[pivot]; aug[pivot] = t; }
      const scale = inv(aug[col][col]);
      for (let j = 0; j < 2 * m; j++) aug[col][j] = mul(aug[col][j], scale);
      for (let r = 0; r < m; r++) {
        if (r === col || !aug[r][col]) continue;
        const f = aug[r][col];
        for (let j = 0; j < 2 * m; j++) aug[r][j] ^= mul(f, aug[col][j]);
      }
    }
    return aug.map(r => r.subarray(m));
  }

  /**
   * Reconstruct missing data chunks.
   *
   * @param data   Array(n) of Uint8Array(size) or null for erasures
   * @param parity Array(k) of Uint8Array(size) or null
   * @returns Array(n) of Uint8Array, or null if unrecoverable
   */
  function decode(data, parity) {
    const n = data.length, k = parity.length;
    const lost = [];
    for (let i = 0; i < n; i++) if (!data[i]) lost.push(i);
    if (lost.length === 0) return data.slice();

    const have = [];
    for (let p = 0; p < k; p++) if (parity[p]) have.push(p);
    if (have.length < lost.length) return null;
    const use = have.slice(0, lost.length);

    let size = 0;
    for (const c of data) if (c) { size = c.length; break; }
    if (!size) for (const c of parity) if (c) { size = c.length; break; }
    if (!size) return null;

    const matrix = cauchyMatrix(n, k);

    // Syndrome: parity row minus the contribution of the data we still hold.
    // What remains is that row's combination of the lost chunks alone.
    const syndromes = use.map(p => {
      const acc = new Uint8Array(size);
      acc.set(parity[p]);
      for (let j = 0; j < n; j++) {
        if (data[j]) maddInto(acc, data[j], matrix[p][j]);
      }
      return acc;
    });

    const sub = use.map(p => {
      const row = new Uint8Array(lost.length);
      for (let c = 0; c < lost.length; c++) row[c] = matrix[p][lost[c]];
      return row;
    });
    const inverse = invertMatrix(sub);
    if (!inverse) return null;

    const out = data.slice();
    for (let r = 0; r < lost.length; r++) {
      const acc = new Uint8Array(size);
      for (let c = 0; c < lost.length; c++) maddInto(acc, syndromes[c], inverse[r][c]);
      out[lost[r]] = acc;
    }
    return out;
  }

  // ── wire format ───────────────────────────────────────────────────────────
  // Mirrors the layout documented in fec.py.
  const VERSION          = 1;
  const HEADER_LEN       = 10;
  const FEC_NONE         = 0;
  const FEC_REED_SOLOMON = 2;
  const MAX_CHUNK_PAYLOAD = 1000;

  /** Parse a chunk header. Returns null for a short buffer or unknown version. */
  function parseHeader(u8) {
    if (u8.byteLength < HEADER_LEN) return null;
    const dv = new DataView(u8.buffer, u8.byteOffset, u8.byteLength);
    const flags = dv.getUint8(0);
    if ((flags & 0x0f) !== VERSION) return null;
    return {
      isKeyframe: (flags & 0x80) !== 0,
      fecType:    (flags >> 4) & 0x07,
      frameId:    dv.getUint16(1),
      chunkIdx:   dv.getUint16(3),
      n:          dv.getUint16(5),
      k:          dv.getUint8(7),
      lastLen:    dv.getUint16(8),
      payload:    u8.subarray(HEADER_LEN),
    };
  }

  /**
   * Wrap-aware signed difference between two uint16 frame ids.
   *
   * Every frame-id comparison must go through this. Using raw unsigned
   * arithmetic is what made the old reassembler delete the frame it was
   * actively assembling whenever a reordered chunk from an older frame arrived.
   */
  function int16Delta(a, b) {
    return (((a - b + 32768) & 0xffff) - 32768);
  }

  const api = {
    decode, parseHeader, cauchyMatrix, int16Delta,
    VERSION, HEADER_LEN, FEC_NONE, FEC_REED_SOLOMON, MAX_CHUNK_PAYLOAD,
  };

  root.DARCFec = api;
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
})(typeof globalThis !== 'undefined' ? globalThis : this);
