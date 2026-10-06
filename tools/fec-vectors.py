#!/usr/bin/env python3
# Copyright 2026 Anton Gravestam
# SPDX-License-Identifier: Apache-2.0
"""
Generate FEC interop test vectors: agent encodes, pilot must decode.

The reference coder (tools/fec-reference/fec.py), seyd-fec (Rust) and
@seyd/core (TypeScript) each implement GF(256) Reed-Solomon independently. If their
field tables, Cauchy construction, or header layout ever drift apart, nothing
fails loudly — the pilot reconstructs plausible-looking wrong bytes and the only
symptom is unexplained video corruption in the field. This harness makes that
failure mode a test.

    python3 tools/fec-vectors.py | node tools/fec-check.js
    python3 tools/fec-vectors.py | cargo run -p seyd-fec --example check
"""

import json
import os
import random
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), 'fec-reference'))

import fec  # noqa: E402


def main():
    seed = int(sys.argv[1]) if len(sys.argv) > 1 else 20260824
    rng = random.Random(seed)

    cases = []
    # Frame sizes chosen to cover the awkward boundaries: exact multiples of the
    # chunk size, one byte over, single-chunk frames, and realistic keyframes.
    lengths = [1, 999, 1000, 1001, 1999, 2000, 7000, 12345, 20000, 40000, 61234]
    percentages = [8, 15, 25, 30, 50]

    for length in lengths:
        for pct in percentages:
            payload = bytes(rng.randrange(256) for _ in range(length))
            is_key = rng.random() < 0.5
            frame_id = rng.randrange(0x10000)
            chunks, n, k = fec.pack_frame(
                payload, frame_id=frame_id, is_keyframe=is_key, fec_pct=pct)

            # Erase as many chunks as the code can theoretically survive — the
            # worst case the pilot must still handle — across data and parity.
            erased = sorted(rng.sample(range(n + k), min(k, n + k)))

            cases.append({
                'label':      f'len={length} pct={pct} n={n} k={k} erased={len(erased)}',
                'n':          n,
                'k':          k,
                'lastLen':    length - (n - 1) * fec.MAX_CHUNK_PAYLOAD,
                'isKeyframe': is_key,
                'frameId':    frame_id,
                'chunks':     [c.hex() for c in chunks],
                'erased':     erased,
                'expected':   payload.hex(),
            })

    json.dump({'seed': seed, 'chunkSize': fec.MAX_CHUNK_PAYLOAD,
               'headerLen': fec.HEADER_LEN, 'version': fec.VERSION,
               'cases': cases}, sys.stdout)
    sys.stdout.write('\n')


if __name__ == '__main__':
    main()
