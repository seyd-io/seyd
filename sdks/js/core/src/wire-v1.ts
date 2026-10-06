// Copyright 2026 Anton Gravestam
// SPDX-License-Identifier: Apache-2.0
// Legacy v1 header (prototype fec.py/fec.js). Decode only, used by the FEC
// interop test until the vector generator emits v2. Not used on the wire.
export const V1_VERSION = 1;
export const V1_HEADER_LEN = 10;

export interface V1Header {
  isKeyframe: boolean; fecType: number; frameId: number; chunkIdx: number;
  n: number; k: number; lastLen: number; payload: Uint8Array;
}

export function parseV1(u8: Uint8Array): V1Header | null {
  if (u8.byteLength < V1_HEADER_LEN) return null;
  const dv = new DataView(u8.buffer, u8.byteOffset, u8.byteLength);
  const flags = dv.getUint8(0);
  if ((flags & 0x0f) !== V1_VERSION) return null;
  return {
    isKeyframe: (flags & 0x80) !== 0, fecType: (flags >> 4) & 7,
    frameId: dv.getUint16(1), chunkIdx: dv.getUint16(3), n: dv.getUint16(5),
    k: dv.getUint8(7), lastLen: dv.getUint16(8), payload: u8.subarray(V1_HEADER_LEN),
  };
}
