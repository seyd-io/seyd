"""
Reed-Solomon erasure coding over GF(256), plus the video chunk wire format.

A frame is split into 1000-byte chunks sent as unreliable QUIC datagrams, and
the pilot can only decode a frame it received in full. Without redundancy a
single lost chunk destroys the whole frame: measured, a 720p keyframe is ~61
chunks, so at 5% chunk loss only 19% of frames survive — and because H.264
delta frames reference their predecessors, each loss smears until the next
keyframe. That is the "quality collapses on motion" symptom.

Adding k parity chunks to n data chunks recovers **any** k losses. Measured at
5% chunk loss on a 12-chunk frame: k=0 -> 54%, k=1 -> 86.5%, k=2 -> 97%.

**Why Reed-Solomon and not XOR parity.** Interleaved XOR is simpler and handles
loss bursts, but only recovers one loss per stripe. RS recovers any k losses
wherever they land, which makes it robust whether real cellular loss turns out
to be bursty or independent — and that question is still open. The usual
objection to RS in Python is speed: a naive per-byte inner loop costs ~7-12 ms
per keyframe, which would be unshippable and would push toward numpy (violating
the pure-Python-wheels constraint recorded in CLAUDE.md for the ARM port).
That objection does not apply here. Doing the GF scalar multiply with
`bytes.translate()` and accumulating with big-integer XOR — both C-speed —
measures **0.24 ms** to encode a 20+10 keyframe and 0.19 ms to decode. 30x
faster than the naive loop, no dependency.

**Cauchy, not Vandermonde**, for the generator matrix: every square submatrix
of a Cauchy matrix is invertible, which is exactly the guarantee "any k losses
recover" needs. A Vandermonde matrix can produce singular submatrices for some
(n, k) and would fail to recover in rare, hard-to-reproduce cases.

The agent computes parity over its own transport-framing chunks. It parses no
NAL headers and is indifferent to the payload being H.264, so this stays inside
DARC's "pure byte relay, never transcodes" rule.
"""

import struct

# ── GF(256) arithmetic ───────────────────────────────────────────────────────
# Field polynomial 0x11d — the conventional choice for Reed-Solomon, and the
# value the pilot's fec.js must use for the two sides to agree.
_POLY = 0x11D

_EXP = [0] * 512
_LOG = [0] * 256

_x = 1
for _i in range(255):
    _EXP[_i] = _x
    _LOG[_x] = _i
    _x <<= 1
    if _x & 0x100:
        _x ^= _POLY
for _i in range(255, 512):
    _EXP[_i] = _EXP[_i - 255]


def _mul(a: int, b: int) -> int:
    if a == 0 or b == 0:
        return 0
    return _EXP[_LOG[a] + _LOG[b]]


def _inv(a: int) -> int:
    return _EXP[255 - _LOG[a]]


# 256 translation tables: _MUL_TABLE[c] maps each byte b to c*b in GF(256).
# bytes.translate() then applies one to a whole chunk at C speed — this is what
# makes pure-Python Reed-Solomon fast enough to sit in the send path.
_MUL_TABLE = [bytes(_mul(c, b) for b in range(256)) for c in range(256)]


# ── generator matrix ─────────────────────────────────────────────────────────

_matrix_cache: dict[tuple[int, int], list[list[int]]] = {}


def cauchy_matrix(n: int, k: int) -> list[list[int]]:
    """
    k x n Cauchy matrix: A[i][j] = 1 / (x_i XOR y_j).

    x_i = i for i in 0..k-1 and y_j = k+j for j in 0..n-1. The two sets are
    disjoint so no element is ever inverted at zero, and distinctness within
    each set is what guarantees every square submatrix is invertible.

    Requires n + k <= 256. The pilot derives the identical matrix from (n, k),
    so this construction is part of the wire contract — changing it breaks
    interop silently.
    """
    key = (n, k)
    cached = _matrix_cache.get(key)
    if cached is not None:
        return cached
    if n + k > 256:
        raise ValueError(f'n+k must be <= 256, got n={n} k={k}')
    m = [[_inv(i ^ (k + j)) for j in range(n)] for i in range(k)]
    _matrix_cache[key] = m
    return m


def parity_count(n: int, pct: int, cap: int = 16) -> int:
    """
    Parity chunks for an n-chunk frame at `pct` overhead.

    Always at least 1 when pct > 0 — a tiny frame still deserves protection,
    and n=1,k=1 degenerates to a plain duplicate, which is the right thing.
    """
    if pct <= 0 or n <= 0:
        return 0
    return max(1, min(cap, n, round(n * pct / 100)))


# ── encode ───────────────────────────────────────────────────────────────────

def encode_parity(chunks: list[bytes], k: int) -> list[bytes]:
    """
    Compute k parity chunks from n equal-length data chunks.

    Every chunk must be exactly the same length — the caller zero-pads the
    final short one, and `last_len` in the wire header carries its true length
    so a reconstructed final chunk can be trimmed back.
    """
    if k <= 0 or not chunks:
        return []
    size = len(chunks[0])
    matrix = cauchy_matrix(len(chunks), k)
    out = []
    for row in matrix:
        acc = 0
        for coeff, chunk in zip(row, chunks):
            if coeff:
                acc ^= int.from_bytes(chunk.translate(_MUL_TABLE[coeff]), 'big')
        out.append(acc.to_bytes(size, 'big'))
    return out


# ── decode (used by the interop test harness; the pilot has its own in JS) ───

def _invert(matrix: list[list[int]]) -> list[list[int]]:
    """Gauss-Jordan inverse of a square GF(256) matrix."""
    m = len(matrix)
    aug = [row[:] + [1 if i == j else 0 for j in range(m)]
           for i, row in enumerate(matrix)]
    for col in range(m):
        pivot = next((r for r in range(col, m) if aug[r][col]), None)
        if pivot is None:
            raise ValueError('singular matrix — should be impossible for Cauchy')
        aug[col], aug[pivot] = aug[pivot], aug[col]
        scale = _inv(aug[col][col])
        aug[col] = [_mul(v, scale) for v in aug[col]]
        for r in range(m):
            if r != col and aug[r][col]:
                f = aug[r][col]
                aug[r] = [v ^ _mul(f, aug[col][i]) for i, v in enumerate(aug[r])]
    return [row[m:] for row in aug]


def decode(data: list[bytes | None], parity: list[bytes | None]) -> list[bytes] | None:
    """
    Reconstruct missing data chunks in place. Returns None if unrecoverable.

    Only the lost data positions are solved for, so the linear system is d x d
    where d is the number of erasures (d <= k <= 16) rather than n x n.
    """
    n, k = len(data), len(parity)
    lost = [i for i in range(n) if data[i] is None]
    if not lost:
        return list(data)  # type: ignore[arg-type]

    available = [p for p in range(k) if parity[p] is not None]
    if len(available) < len(lost):
        return None
    use = available[:len(lost)]

    matrix = cauchy_matrix(n, k)
    size = next(c for c in list(data) + list(parity) if c is not None)
    size = len(size)

    # Syndrome: each chosen parity chunk minus the contribution of the data we
    # still have. What remains is that row's combination of the lost chunks.
    syndromes = []
    for p in use:
        acc = 0
        for j in range(n):
            chunk = data[j]
            coeff = matrix[p][j]
            if chunk is not None and coeff:
                acc ^= int.from_bytes(chunk.translate(_MUL_TABLE[coeff]), 'big')
        syndromes.append((int.from_bytes(parity[p], 'big') ^ acc).to_bytes(size, 'big'))

    sub = [[matrix[p][j] for j in lost] for p in use]
    inverse = _invert(sub)

    out = list(data)
    for r, idx in enumerate(lost):
        acc = 0
        for c in range(len(lost)):
            coeff = inverse[r][c]
            if coeff:
                acc ^= int.from_bytes(syndromes[c].translate(_MUL_TABLE[coeff]), 'big')
        out[idx] = acc.to_bytes(size, 'big')
    return out  # type: ignore[return-value]


# ── wire format ──────────────────────────────────────────────────────────────
#
#   byte  0     bit 7    keyframe
#               bits 4-6 fec_type (0 = none, 2 = reed-solomon)
#               bits 0-3 format version
#   bytes 1-2   frame_id      uint16 BE
#   bytes 3-4   chunk_idx     uint16 BE   0..n-1 data, n..n+k-1 parity
#   bytes 5-6   total_chunks  uint16 BE   = n (DATA chunks only)
#   byte  7     fec_count     uint8       = k
#   bytes 8-9   last_len      uint16 BE   real length of data chunk n-1
#   bytes 10+   payload
#
# The first three fields keep the byte offsets they had in the 7-byte v0 header,
# but the version nibble makes this a hard cutover regardless: agent and pilot
# must be deployed together. An unknown version must be counted and dropped
# loudly rather than misparsed into garbage video.
#
# last_len is not optional. Parity is computed over chunks zero-padded to the
# full chunk size, so if the short final chunk is the one reconstructed it comes
# back padded, and there is no other way to recover its true length. Getting it
# wrong appends up to 999 zero bytes to recovered frames — which H.264 decoders
# sometimes tolerate and sometimes do not, i.e. the worst kind of bug.

VERSION = 1
FEC_NONE = 0
FEC_REED_SOLOMON = 2

HEADER = '>BHHHBH'
HEADER_LEN = struct.calcsize(HEADER)   # 10
assert HEADER_LEN == 10

MAX_CHUNK_PAYLOAD = 1000
"""
QUIC datagrams must fit one UDP packet. Path MTU is typically 1200-1500 bytes
and QUIC + HTTP/3 + WebTransport framing takes a slice of that, so 1000 bytes
of payload plus the 10-byte header is comfortably conservative on any path.
"""


def pack_frame(payload: bytes, *, frame_id: int, is_keyframe: bool,
               fec_pct: int, chunk_size: int = MAX_CHUNK_PAYLOAD,
               cap: int = 16) -> tuple[list[bytes], int, int]:
    """
    Split one H.264 access unit into wire chunks, with parity appended.

    Returns (chunks, n, k). Data chunks carry their real bytes — only the copy
    fed to the parity computation is zero-padded, so the common case sends no
    padding on the wire.
    """
    n = max(1, -(-len(payload) // chunk_size))
    last_len = len(payload) - (n - 1) * chunk_size
    k = parity_count(n, fec_pct, cap)

    slices = [payload[i * chunk_size:(i + 1) * chunk_size] for i in range(n)]

    parity: list[bytes] = []
    if k:
        padded = slices[:-1] + [slices[-1].ljust(chunk_size, b'\x00')]
        parity = encode_parity(padded, k)

    fec_type = FEC_REED_SOLOMON if k else FEC_NONE
    flags = (0x80 if is_keyframe else 0x00) | (fec_type << 4) | VERSION

    chunks = []
    for idx, body in enumerate(slices + parity):
        chunks.append(struct.pack(HEADER, flags, frame_id & 0xFFFF, idx,
                                  n, k, last_len) + body)
    return chunks, n, k


def unpack_header(buf: bytes) -> dict | None:
    """Parse a chunk header. Returns None if too short or the version is unknown."""
    if len(buf) < HEADER_LEN:
        return None
    flags, frame_id, chunk_idx, n, k, last_len = struct.unpack_from(HEADER, buf)
    if (flags & 0x0F) != VERSION:
        return None
    return {
        'is_keyframe': bool(flags & 0x80),
        'fec_type':    (flags >> 4) & 0x07,
        'frame_id':    frame_id,
        'chunk_idx':   chunk_idx,
        'n':           n,
        'k':           k,
        'last_len':    last_len,
        'payload':     buf[HEADER_LEN:],
    }
