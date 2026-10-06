// Copyright 2026 Anton Gravestam
// SPDX-License-Identifier: Apache-2.0
//! Wire protocol v2 chunk header and frame metadata (ADR 0001).
//!
//! ```text
//! byte  0      bit 7 keyframe | bits 4-6 fec_type | bits 0-3 version = 2
//! byte  1      channel_id   u8
//! bytes 2-3    frame_id     u16 BE   per channel, wraps
//! bytes 4-5    chunk_idx    u16 BE   0..n-1 data, n..n+k-1 parity, within the block
//! bytes 6-7    n            u16 BE   data chunks in this FEC block
//! byte  8      k            u8       parity chunks in this FEC block
//! byte  9      flags2       bit0 frame_meta, bit1 discardable, bit2 end_of_frame
//! bytes 10-11  last_len     u16 BE   real length of data chunk n-1 of this block
//! bytes 12-13  chunk_len    u16 BE   payload size of full chunks in this block
//! bytes 14-17  send_ts      u32 BE   low 32 bits of sender monotonic µs
//! bytes 18-19  block_idx    u16 BE   FEC block index within the frame
//! bytes 20+    payload
//! ```

pub const VERSION: u8 = 2;
pub const HEADER_LEN: usize = 20;
pub const CONTROL_CHANNEL: u8 = 0;

/// Default chunk payload before path-MTU discovery raises it.
pub const DEFAULT_CHUNK_LEN: u16 = 1000;
/// Upper bound the sender will ever use; keeps a chunk inside one 1400-byte
/// QUIC packet after QUIC + HTTP/3 + WebTransport framing.
pub const MAX_CHUNK_LEN: u16 = 1350;

pub const FLAG2_FRAME_META: u8 = 1 << 0;
pub const FLAG2_DISCARDABLE: u8 = 1 << 1;
pub const FLAG2_END_OF_FRAME: u8 = 1 << 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FecType {
    None = 0,
    ReedSolomon = 2,
}

impl FecType {
    fn from_bits(b: u8) -> Option<Self> {
        match b {
            0 => Some(FecType::None),
            2 => Some(FecType::ReedSolomon),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkHeader {
    pub keyframe: bool,
    pub fec_type: FecType,
    pub channel_id: u8,
    pub frame_id: u16,
    pub chunk_idx: u16,
    pub n: u16,
    pub k: u8,
    pub flags2: u8,
    pub last_len: u16,
    pub chunk_len: u16,
    pub send_ts: u32,
    pub block_idx: u16,
}

impl ChunkHeader {
    #[inline]
    pub fn is_parity(&self) -> bool {
        self.chunk_idx >= self.n
    }
    #[inline]
    pub fn has_frame_meta(&self) -> bool {
        self.flags2 & FLAG2_FRAME_META != 0
    }
    #[inline]
    pub fn is_discardable(&self) -> bool {
        self.flags2 & FLAG2_DISCARDABLE != 0
    }
    #[inline]
    pub fn is_end_of_frame(&self) -> bool {
        self.flags2 & FLAG2_END_OF_FRAME != 0
    }

    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut out = [0u8; HEADER_LEN];
        out[0] = (if self.keyframe { 0x80 } else { 0 }) | ((self.fec_type as u8) << 4) | VERSION;
        out[1] = self.channel_id;
        out[2..4].copy_from_slice(&self.frame_id.to_be_bytes());
        out[4..6].copy_from_slice(&self.chunk_idx.to_be_bytes());
        out[6..8].copy_from_slice(&self.n.to_be_bytes());
        out[8] = self.k;
        out[9] = self.flags2;
        out[10..12].copy_from_slice(&self.last_len.to_be_bytes());
        out[12..14].copy_from_slice(&self.chunk_len.to_be_bytes());
        out[14..18].copy_from_slice(&self.send_ts.to_be_bytes());
        out[18..20].copy_from_slice(&self.block_idx.to_be_bytes());
        out
    }

    /// Parse a v2 chunk. `None` if too short, wrong version, or an unknown FEC type.
    pub fn parse(buf: &[u8]) -> Option<(ChunkHeader, &[u8])> {
        if buf.len() < HEADER_LEN {
            return None;
        }
        let flags = buf[0];
        if flags & 0x0f != VERSION {
            return None;
        }
        let fec_type = FecType::from_bits((flags >> 4) & 0x07)?;
        let be16 = |i: usize| u16::from_be_bytes([buf[i], buf[i + 1]]);
        let hdr = ChunkHeader {
            keyframe: flags & 0x80 != 0,
            fec_type,
            channel_id: buf[1],
            frame_id: be16(2),
            chunk_idx: be16(4),
            n: be16(6),
            k: buf[8],
            flags2: buf[9],
            last_len: be16(10),
            chunk_len: be16(12),
            send_ts: u32::from_be_bytes([buf[14], buf[15], buf[16], buf[17]]),
            block_idx: be16(18),
        };
        if hdr.n == 0 || hdr.chunk_idx as usize >= hdr.n as usize + hdr.k as usize {
            return None;
        }
        Some((hdr, &buf[HEADER_LEN..]))
    }
}

/// Optional per-frame metadata carried at the start of block 0 / chunk 0's
/// payload when `FLAG2_FRAME_META` is set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameMeta {
    /// Publisher capture timestamp, its own monotonic clock, microseconds.
    pub capture_ts_us: u64,
    /// Position within the GOP (0 = keyframe).
    pub seq_in_gop: u16,
}

impl FrameMeta {
    pub const LEN: usize = 10;

    pub fn encode(&self) -> [u8; Self::LEN] {
        let mut out = [0u8; Self::LEN];
        out[..8].copy_from_slice(&self.capture_ts_us.to_be_bytes());
        out[8..].copy_from_slice(&self.seq_in_gop.to_be_bytes());
        out
    }

    pub fn parse(buf: &[u8]) -> Option<(FrameMeta, &[u8])> {
        if buf.len() < Self::LEN {
            return None;
        }
        let mut ts = [0u8; 8];
        ts.copy_from_slice(&buf[..8]);
        Some((
            FrameMeta {
                capture_ts_us: u64::from_be_bytes(ts),
                seq_in_gop: u16::from_be_bytes([buf[8], buf[9]]),
            },
            &buf[Self::LEN..],
        ))
    }
}

/// Wrap-aware difference of two 32-bit microsecond send timestamps.
#[inline]
pub fn ts_delta_us(later: u32, earlier: u32) -> i32 {
    later.wrapping_sub(earlier) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ChunkHeader {
        ChunkHeader {
            keyframe: true,
            fec_type: FecType::ReedSolomon,
            channel_id: 3,
            frame_id: 0xbeef,
            chunk_idx: 9,
            n: 8,
            k: 4,
            flags2: FLAG2_FRAME_META | FLAG2_END_OF_FRAME,
            last_len: 517,
            chunk_len: 1200,
            send_ts: 0xdeadbeef,
            block_idx: 2,
        }
    }

    #[test]
    fn roundtrip() {
        let h = sample();
        let mut buf = h.encode().to_vec();
        buf.extend_from_slice(b"payload");
        let (parsed, payload) = ChunkHeader::parse(&buf).unwrap();
        assert_eq!(parsed, h);
        assert_eq!(payload, b"payload");
        assert!(parsed.is_parity());
        assert!(parsed.has_frame_meta());
        assert!(parsed.is_end_of_frame());
        assert!(!parsed.is_discardable());
    }

    #[test]
    fn layout_is_exact() {
        let bytes = sample().encode();
        assert_eq!(bytes[0], 0x80 | (2 << 4) | 2);
        assert_eq!(bytes[1], 3);
        assert_eq!(&bytes[2..4], &[0xbe, 0xef]);
        assert_eq!(&bytes[6..8], &[0, 8]);
        assert_eq!(bytes[8], 4);
        assert_eq!(&bytes[14..18], &[0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(&bytes[18..20], &[0, 2]);
    }

    #[test]
    fn rejects_bad_input() {
        let mut bytes = sample().encode();
        assert!(ChunkHeader::parse(&bytes[..19]).is_none(), "short");
        bytes[0] = (bytes[0] & 0xf0) | 1;
        assert!(
            ChunkHeader::parse(&bytes).is_none(),
            "v1 not accepted by v2 parser"
        );
        let mut bytes = sample().encode();
        bytes[0] = (bytes[0] & 0x8f) | (5 << 4);
        assert!(ChunkHeader::parse(&bytes).is_none(), "unknown fec type");
        let mut bytes = sample().encode();
        bytes[4..6].copy_from_slice(&12u16.to_be_bytes());
        assert!(ChunkHeader::parse(&bytes).is_none(), "chunk_idx >= n+k");
    }

    #[test]
    fn frame_meta_roundtrip() {
        let m = FrameMeta {
            capture_ts_us: 1_700_000_000_123_456,
            seq_in_gop: 7,
        };
        let mut buf = m.encode().to_vec();
        buf.push(0x42);
        let (p, rest) = FrameMeta::parse(&buf).unwrap();
        assert_eq!(p, m);
        assert_eq!(rest, &[0x42]);
    }

    #[test]
    fn ts_delta_wraps() {
        assert_eq!(ts_delta_us(10, u32::MAX - 5), 16);
        assert_eq!(ts_delta_us(5, 10), -5);
    }
}
