//! Frames → wire chunks (docs/protocol/chunks.md).
//!
//! A video frame becomes one or more FEC **blocks** of up to
//! `BLOCK_DATA_CHUNKS` data chunks plus `k` parity chunks computed over that
//! block only. The packer is pure: it allocates the chunk buffers and returns
//! them; the sender decides admission and transmits.

use bytes::{BufMut, Bytes, BytesMut};
use seyd_wire::v2::{self, ChunkHeader, FecType, FrameMeta, HEADER_LEN};

pub const BLOCK_DATA_CHUNKS: usize = 8;
pub const MAX_PARITY_PER_BLOCK: usize = 16;

pub struct VideoPack {
    pub chunks: Vec<Bytes>,
    pub blocks: usize,
    pub data_chunks: usize,
    pub parity_chunks: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct FrameParams {
    pub channel_id: u8,
    pub frame_id: u16,
    pub keyframe: bool,
    /// The profile's delta or key FEC percentage.
    pub fec_pct: u32,
    pub chunk_len: u16,
    pub send_ts: u32,
    pub meta: Option<FrameMeta>,
}

/// Pack one access unit.
pub fn pack_video(p: FrameParams, payload: &[u8]) -> VideoPack {
    let FrameParams {
        channel_id,
        frame_id,
        keyframe,
        fec_pct,
        chunk_len,
        send_ts,
        meta,
    } = p;
    let chunk_len_us = chunk_len as usize;
    // Frame meta is prepended to the payload of block 0 / chunk 0 and is
    // part of the chunked byte stream, so it is FEC-protected like the rest.
    let mut body: Vec<u8>;
    let bytes: &[u8] = if let Some(m) = meta {
        body = Vec::with_capacity(payload.len() + FrameMeta::LEN);
        body.extend_from_slice(&m.encode());
        body.extend_from_slice(payload);
        &body
    } else {
        payload
    };

    let total_data = bytes.len().div_ceil(chunk_len_us).max(1);
    let blocks = total_data.div_ceil(BLOCK_DATA_CHUNKS);
    let mut out = Vec::with_capacity(total_data + blocks * 4);
    let (mut data_count, mut parity_count_total) = (0, 0);

    for block_idx in 0..blocks {
        let first = block_idx * BLOCK_DATA_CHUNKS;
        let n = (total_data - first).min(BLOCK_DATA_CHUNKS);
        let last = block_idx + 1 == blocks;
        let k = seyd_fec::parity_count(n, fec_pct, MAX_PARITY_PER_BLOCK);

        let slices: Vec<&[u8]> = (0..n)
            .map(|i| {
                let start = (first + i) * chunk_len_us;
                let end = (start + chunk_len_us).min(bytes.len());
                if start >= bytes.len() {
                    &[][..]
                } else {
                    &bytes[start..end]
                }
            })
            .collect();
        let last_len = slices[n - 1].len() as u16;

        let mut flags2 = 0u8;
        if block_idx == 0 && meta.is_some() {
            flags2 |= v2::FLAG2_FRAME_META;
        }
        if last {
            flags2 |= v2::FLAG2_END_OF_FRAME;
        }
        let hdr = ChunkHeader {
            keyframe,
            fec_type: if k > 0 {
                FecType::ReedSolomon
            } else {
                FecType::None
            },
            channel_id,
            frame_id,
            chunk_idx: 0,
            n: n as u16,
            k: k as u8,
            flags2,
            last_len,
            chunk_len,
            send_ts,
            block_idx: block_idx as u16,
        };

        for (i, s) in slices.iter().enumerate() {
            let mut b = BytesMut::with_capacity(HEADER_LEN + s.len());
            b.put_slice(
                &ChunkHeader {
                    chunk_idx: i as u16,
                    ..hdr
                }
                .encode(),
            );
            b.put_slice(s);
            out.push(b.freeze());
        }
        data_count += n;

        if k > 0 {
            // Parity over zero-padded copies; data chunks go unpadded on the wire.
            let padded: Vec<Vec<u8>> = slices
                .iter()
                .map(|s| {
                    let mut v = s.to_vec();
                    v.resize(chunk_len_us, 0);
                    v
                })
                .collect();
            let refs: Vec<&[u8]> = padded.iter().map(|v| v.as_slice()).collect();
            for (j, p) in seyd_fec::encode_parity(&refs, k).into_iter().enumerate() {
                let mut b = BytesMut::with_capacity(HEADER_LEN + p.len());
                b.put_slice(
                    &ChunkHeader {
                        chunk_idx: (n + j) as u16,
                        ..hdr
                    }
                    .encode(),
                );
                b.put_slice(&p);
                out.push(b.freeze());
            }
            parity_count_total += k;
        }
    }
    VideoPack {
        chunks: out,
        blocks,
        data_chunks: data_count,
        parity_chunks: parity_count_total,
    }
}

/// Pack one sensor/command message: `n = 1, k = 0`, END_OF_FRAME.
pub fn pack_message(channel_id: u8, seq: u16, send_ts: u32, payload: &[u8]) -> Bytes {
    let len = payload.len().min(u16::MAX as usize) as u16;
    let hdr = ChunkHeader {
        keyframe: false,
        fec_type: FecType::None,
        channel_id,
        frame_id: seq,
        chunk_idx: 0,
        n: 1,
        k: 0,
        flags2: v2::FLAG2_END_OF_FRAME,
        last_len: len,
        chunk_len: len,
        send_ts,
        block_idx: 0,
    };
    let mut b = BytesMut::with_capacity(HEADER_LEN + payload.len());
    b.put_slice(&hdr.encode());
    b.put_slice(&payload[..len as usize]);
    b.freeze()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reference receiver used only to validate the packer: reassembles from
    /// chunks, recovering each block through seyd-fec after erasures.
    fn reassemble(chunks: &[Bytes], erase: &[usize]) -> Option<(Vec<u8>, bool)> {
        use std::collections::BTreeMap;
        type Block = (ChunkHeader, Vec<Option<Vec<u8>>>, Vec<Option<Vec<u8>>>);
        let mut blocks: BTreeMap<u16, Block> = BTreeMap::new();
        let mut end_seen = None;
        for (i, c) in chunks.iter().enumerate() {
            if erase.contains(&i) {
                continue;
            }
            let (h, p) = ChunkHeader::parse(c).unwrap();
            let e = blocks
                .entry(h.block_idx)
                .or_insert_with(|| (h, vec![None; h.n as usize], vec![None; h.k as usize]));
            if h.is_end_of_frame() {
                end_seen = Some(h.block_idx);
            }
            if h.is_parity() {
                e.2[(h.chunk_idx - h.n) as usize] = Some(p.to_vec());
            } else {
                let mut v = p.to_vec();
                v.resize(h.chunk_len as usize, 0);
                e.1[h.chunk_idx as usize] = Some(v);
            }
        }
        let last = end_seen?;
        let mut out = Vec::new();
        let mut key = false;
        for b in 0..=last {
            let (h, data, parity) = blocks.get_mut(&b)?;
            key = h.keyframe;
            if !seyd_fec::decode(data, parity) {
                return None;
            }
            for (i, d) in data.iter().enumerate() {
                let take = if i + 1 == h.n as usize {
                    h.last_len as usize
                } else {
                    h.chunk_len as usize
                };
                out.extend_from_slice(&d.as_ref().unwrap()[..take]);
            }
        }
        Some((out, key))
    }

    #[test]
    fn multi_block_keyframe_survives_k_losses_per_block() {
        let payload: Vec<u8> = (0..23_456u32).map(|i| (i * 7 % 251) as u8).collect();
        let pack = pack_video(
            FrameParams {
                channel_id: 1,
                frame_id: 42,
                keyframe: true,
                fec_pct: 50,
                chunk_len: 1000,
                send_ts: 123,
                meta: None,
            },
            &payload,
        );
        // 24 data chunks → 3 blocks of 8, each with k = 4 at 50 %
        assert_eq!(pack.blocks, 3);
        assert_eq!(pack.data_chunks, 24);
        assert_eq!(pack.parity_chunks, 12);
        assert_eq!(pack.chunks.len(), 36);
        // erase 4 chunks from each block (block = 12 chunks on the wire)
        let erase: Vec<usize> = [0usize, 5, 8, 11, 12, 13, 22, 23, 24, 30, 33, 35].to_vec();
        let (out, key) = reassemble(&pack.chunks, &erase).unwrap();
        assert!(key);
        assert_eq!(out, payload);
        // one more loss in a block is unrecoverable
        let mut erase5 = erase.clone();
        erase5.push(1);
        assert!(reassemble(&pack.chunks, &erase5).is_none());
    }

    #[test]
    fn tiny_frame_is_one_block_with_meta() {
        let meta = FrameMeta {
            capture_ts_us: 99,
            seq_in_gop: 3,
        };
        let pack = pack_video(
            FrameParams {
                channel_id: 2,
                frame_id: 7,
                keyframe: false,
                fec_pct: 15,
                chunk_len: 1000,
                send_ts: 5,
                meta: Some(meta),
            },
            b"hello",
        );
        assert_eq!(pack.chunks.len(), 2); // 1 data + k=1 (floor of 1)
        let (h, p) = ChunkHeader::parse(&pack.chunks[0]).unwrap();
        assert!(h.has_frame_meta() && h.is_end_of_frame() && !h.keyframe);
        assert_eq!(h.last_len as usize, FrameMeta::LEN + 5);
        let (m, rest) = FrameMeta::parse(p).unwrap();
        assert_eq!(m, meta);
        assert_eq!(rest, b"hello");
        let (out, _) = reassemble(&pack.chunks, &[0]).unwrap();
        assert_eq!(&out[FrameMeta::LEN..], b"hello");
    }

    #[test]
    fn exact_multiple_has_full_last_len() {
        let payload = vec![1u8; 8000];
        let pack = pack_video(
            FrameParams {
                channel_id: 1,
                frame_id: 1,
                keyframe: false,
                fec_pct: 0,
                chunk_len: 1000,
                send_ts: 0,
                meta: None,
            },
            &payload,
        );
        assert_eq!(pack.chunks.len(), 8);
        let (h, _) = ChunkHeader::parse(&pack.chunks[7]).unwrap();
        assert_eq!(h.last_len, 1000);
        assert_eq!(h.fec_type, FecType::None);
    }

    #[test]
    fn message_pack() {
        let m = pack_message(3, 65535, 1, b"{\"pan\":1}");
        let (h, p) = ChunkHeader::parse(&m).unwrap();
        assert_eq!((h.n, h.k, h.frame_id, h.channel_id), (1, 0, 65535, 3));
        assert!(h.is_end_of_frame());
        assert_eq!(p, b"{\"pan\":1}");
    }
}
