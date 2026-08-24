#!/usr/bin/env node
// Replay FEC interop vectors from tools/fec-vectors.py through the pilot's decoder.
//
//     python3 tools/fec-vectors.py | node tools/fec-check.js
//
// Verifies that the pilot's independent GF(256) implementation agrees with the
// agent's byte for byte — field tables, Cauchy matrix, header layout, and the
// last_len trimming that a reconstructed final chunk depends on.

const path = require('path');
const FEC = require(path.join(__dirname, '..', 'packages', 'pilot', 'fec.js'));

function readStdin() {
  return new Promise((resolve, reject) => {
    let buf = '';
    process.stdin.setEncoding('utf8');
    process.stdin.on('data', d => { buf += d; });
    process.stdin.on('end', () => resolve(buf));
    process.stdin.on('error', reject);
  });
}

function hexToBytes(hex) {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(hex.substr(i * 2, 2), 16);
  return out;
}

(async () => {
  const raw = await readStdin();
  if (!raw.trim()) {
    console.error('no input — pipe tools/fec-vectors.py into this script');
    process.exit(2);
  }
  const vec = JSON.parse(raw);

  if (vec.headerLen !== FEC.HEADER_LEN || vec.version !== FEC.VERSION) {
    console.error(`FAIL wire mismatch: agent headerLen=${vec.headerLen} v${vec.version}, ` +
                  `pilot headerLen=${FEC.HEADER_LEN} v${FEC.VERSION}`);
    process.exit(1);
  }

  let pass = 0, fail = 0;
  for (const c of vec.cases) {
    const erased = new Set(c.erased);
    const data = new Array(c.n).fill(null);
    const parity = new Array(c.k).fill(null);
    let headerOk = true;

    c.chunks.forEach((hex, i) => {
      if (erased.has(i)) return;
      const h = FEC.parseHeader(hexToBytes(hex));
      if (!h) { headerOk = false; return; }
      if (h.n !== c.n || h.k !== c.k || h.lastLen !== c.lastLen ||
          h.frameId !== c.frameId || h.isKeyframe !== c.isKeyframe) {
        headerOk = false;
        return;
      }
      if (h.chunkIdx < c.n) {
        // Parity was computed over zero-padded chunks, so the short final data
        // chunk must be re-padded before it can take part in reconstruction.
        let body = h.payload;
        if (body.length < vec.chunkSize) {
          const padded = new Uint8Array(vec.chunkSize);
          padded.set(body);
          body = padded;
        }
        data[h.chunkIdx] = body;
      } else {
        parity[h.chunkIdx - c.n] = h.payload;
      }
    });

    if (!headerOk) {
      console.error(`FAIL header  ${c.label}`);
      fail++;
      continue;
    }

    const out = FEC.decode(data, parity);
    if (!out) {
      console.error(`FAIL unrecoverable  ${c.label}`);
      fail++;
      continue;
    }

    // Reassemble exactly as pilot.js does: full chunks, then trim the last to
    // lastLen. This is where a missing last_len would silently corrupt.
    const total = (c.n - 1) * vec.chunkSize + c.lastLen;
    const rebuilt = new Uint8Array(total);
    let off = 0;
    for (let i = 0; i < c.n; i++) {
      const take = (i === c.n - 1) ? c.lastLen : vec.chunkSize;
      rebuilt.set(out[i].subarray(0, take), off);
      off += take;
    }

    const expected = hexToBytes(c.expected);
    let same = rebuilt.length === expected.length;
    if (same) {
      for (let i = 0; i < expected.length; i++) {
        if (rebuilt[i] !== expected[i]) { same = false; break; }
      }
    }
    if (same) pass++;
    else { console.error(`FAIL mismatch  ${c.label}`); fail++; }
  }

  console.log(`fec interop: ${pass} passed, ${fail} failed (seed ${vec.seed})`);
  process.exit(fail ? 1 : 0);
})();
