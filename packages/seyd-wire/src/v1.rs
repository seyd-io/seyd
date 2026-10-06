// Copyright 2026 Anton Gravestam
// SPDX-License-Identifier: Apache-2.0
//! Legacy v1 header (prototype, `packages/agent/fec.py`). Decode only.
//!
//! Kept so the Rust coder can be checked against the Python-generated interop
//! vectors, and so a v2 receiver can *recognise* v1 traffic in its
//! `bad_header` counter. Not used on the wire by the product.

pub const VERSION: u8 = 1;
pub const HEADER_LEN: usize = 10;
pub const MAX_CHUNK_PAYLOAD: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub keyframe: bool,
    pub fec_type: u8,
    pub frame_id: u16,
    pub chunk_idx: u16,
    pub n: u16,
    pub k: u8,
    pub last_len: u16,
}

/// Parse a v1 chunk. `None` if too short or not version 1.
pub fn parse(buf: &[u8]) -> Option<(Header, &[u8])> {
    if buf.len() < HEADER_LEN {
        return None;
    }
    let flags = buf[0];
    if flags & 0x0f != VERSION {
        return None;
    }
    let be16 = |i: usize| u16::from_be_bytes([buf[i], buf[i + 1]]);
    Some((
        Header {
            keyframe: flags & 0x80 != 0,
            fec_type: (flags >> 4) & 0x07,
            frame_id: be16(1),
            chunk_idx: be16(3),
            n: be16(5),
            k: buf[7],
            last_len: be16(8),
        },
        &buf[HEADER_LEN..],
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_reference_layout() {
        // flags: keyframe | RS | v1 ; frame 0x1234 ; idx 3 ; n 5 ; k 2 ; last_len 999
        let buf = [
            0xa1, 0x12, 0x34, 0x00, 0x03, 0x00, 0x05, 0x02, 0x03, 0xe7, 0xaa,
        ];
        let (h, payload) = parse(&buf).unwrap();
        assert_eq!(
            h,
            Header {
                keyframe: true,
                fec_type: 2,
                frame_id: 0x1234,
                chunk_idx: 3,
                n: 5,
                k: 2,
                last_len: 999
            }
        );
        assert_eq!(payload, &[0xaa]);
        assert!(parse(&[0xa2; 10]).is_none(), "wrong version rejected");
        assert!(parse(&[0xa1; 9]).is_none(), "short buffer rejected");
    }
}
