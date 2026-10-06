// Copyright 2026 Anton Gravestam
// SPDX-License-Identifier: Apache-2.0
// Wire protocol v2 — docs/adr/0001-wire-protocol-v2.md, packages/seyd-wire/src/v2.rs.
export const VERSION = 2;
export const HEADER_LEN = 20;
export const CONTROL_CHANNEL = 0;
export const DEFAULT_CHUNK_LEN = 1000;
export const MAX_CHUNK_LEN = 1350;

export const FEC_NONE = 0;
export const FEC_REED_SOLOMON = 2;

export const FLAG2_FRAME_META = 1 << 0;
export const FLAG2_DISCARDABLE = 1 << 1;
export const FLAG2_END_OF_FRAME = 1 << 2;

export interface ChunkHeader {
  keyframe: boolean;
  fecType: number;
  channelId: number;
  frameId: number;
  chunkIdx: number;
  n: number;
  k: number;
  flags2: number;
  lastLen: number;
  chunkLen: number;
  sendTs: number;   // u32, low 32 bits of agent monotonic µs
  blockIdx: number;
}

export interface ParsedChunk {
  header: ChunkHeader;
  payload: Uint8Array;
}

export function encodeHeader(h: ChunkHeader, out?: Uint8Array): Uint8Array {
  const buf = out ?? new Uint8Array(HEADER_LEN);
  const dv = new DataView(buf.buffer, buf.byteOffset, HEADER_LEN);
  dv.setUint8(0, (h.keyframe ? 0x80 : 0) | ((h.fecType & 7) << 4) | VERSION);
  dv.setUint8(1, h.channelId);
  dv.setUint16(2, h.frameId & 0xffff);
  dv.setUint16(4, h.chunkIdx);
  dv.setUint16(6, h.n);
  dv.setUint8(8, h.k);
  dv.setUint8(9, h.flags2);
  dv.setUint16(10, h.lastLen);
  dv.setUint16(12, h.chunkLen);
  dv.setUint32(14, h.sendTs >>> 0);
  dv.setUint16(18, h.blockIdx);
  return buf;
}

/** Parse a v2 chunk. null if short, wrong version, unknown FEC type, or inconsistent. */
export function parseChunk(u8: Uint8Array): ParsedChunk | null {
  if (u8.byteLength < HEADER_LEN) return null;
  const dv = new DataView(u8.buffer, u8.byteOffset, u8.byteLength);
  const flags = dv.getUint8(0);
  if ((flags & 0x0f) !== VERSION) return null;
  const fecType = (flags >> 4) & 7;
  if (fecType !== FEC_NONE && fecType !== FEC_REED_SOLOMON) return null;
  const h: ChunkHeader = {
    keyframe: (flags & 0x80) !== 0,
    fecType,
    channelId: dv.getUint8(1),
    frameId: dv.getUint16(2),
    chunkIdx: dv.getUint16(4),
    n: dv.getUint16(6),
    k: dv.getUint8(8),
    flags2: dv.getUint8(9),
    lastLen: dv.getUint16(10),
    chunkLen: dv.getUint16(12),
    sendTs: dv.getUint32(14),
    blockIdx: dv.getUint16(18),
  };
  if (h.n === 0 || h.chunkIdx >= h.n + h.k) return null;
  return { header: h, payload: u8.subarray(HEADER_LEN) };
}

export interface FrameMeta {
  captureTsUs: bigint;
  seqInGop: number;
}
export const FRAME_META_LEN = 10;

export function encodeFrameMeta(m: FrameMeta): Uint8Array {
  const buf = new Uint8Array(FRAME_META_LEN);
  const dv = new DataView(buf.buffer);
  dv.setBigUint64(0, m.captureTsUs);
  dv.setUint16(8, m.seqInGop);
  return buf;
}

export function parseFrameMeta(u8: Uint8Array): { meta: FrameMeta; rest: Uint8Array } | null {
  if (u8.byteLength < FRAME_META_LEN) return null;
  const dv = new DataView(u8.buffer, u8.byteOffset, u8.byteLength);
  return {
    meta: { captureTsUs: dv.getBigUint64(0), seqInGop: dv.getUint16(8) },
    rest: u8.subarray(FRAME_META_LEN),
  };
}

/** Wrap-aware signed difference of two uint16 sequence numbers (a - b). */
export function seqDelta(a: number, b: number): number {
  return (((a - b + 32768) & 0xffff) - 32768);
}

/** Wrap-aware signed difference of two uint32 microsecond timestamps. */
export function tsDeltaUs(later: number, earlier: number): number {
  return ((later - earlier) | 0);
}

/** Build a single-chunk message datagram (sensor/command channels: n=1, k=0). */
export function encodeMessage(channelId: number, seq: number, payload: Uint8Array, sendTsUs: number): Uint8Array {
  const out = new Uint8Array(HEADER_LEN + payload.length);
  encodeHeader({
    keyframe: false, fecType: FEC_NONE, channelId, frameId: seq & 0xffff, chunkIdx: 0,
    n: 1, k: 0, flags2: FLAG2_END_OF_FRAME, lastLen: payload.length, chunkLen: payload.length,
    sendTs: sendTsUs >>> 0, blockIdx: 0,
  }, out);
  out.set(payload, HEADER_LEN);
  return out;
}
