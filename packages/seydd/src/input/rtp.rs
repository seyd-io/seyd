//! Bare RTP/H.264 over UDP (RFC 6184) — what `sim/video-source.sh` and most
//! GStreamer/FFmpeg pipelines emit. In-house depacketizer: single NAL units,
//! STAP-A, FU-A; access units delimited by the RTP marker bit or a timestamp
//! change. Emits Annex B. No decoding.

use super::VideoAu;
use bytes::{BufMut, Bytes, BytesMut};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

const START_CODE: [u8; 4] = [0, 0, 0, 1];

pub async fn run(addr: String, tx: mpsc::Sender<VideoAu>) {
    let sock = match UdpSocket::bind(&addr).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(%addr, error = %e, "rtp input: bind failed");
            return;
        }
    };
    tracing::info!(%addr, "rtp input listening");
    let mut buf = vec![0u8; 65536];
    let mut dp = Depacketizer::default();
    loop {
        let n = match sock.recv(&mut buf).await {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(error = %e, "rtp recv");
                continue;
            }
        };
        if let Some(au) = dp.push(&buf[..n]) {
            if tx.send(au).await.is_err() {
                return;
            }
        }
    }
}

/// H.264 RTP sampling clock (RFC 6184 §8.2.1).
const RTP_CLOCK_HZ: u64 = 90_000;

#[derive(Default)]
pub struct Depacketizer {
    au: BytesMut,
    au_ts: Option<u32>,
    /// `au_ts` unwrapped past the 32-bit rollover (~13 h at 90 kHz), relative
    /// to the first packet — the source's sampling timeline, which the pilot
    /// paces presentation on.
    au_ts_ext: u64,
    ts_base: Option<u32>,
    ts_wraps: u64,
    keyframe: bool,
    fu: Option<BytesMut>,
    last_seq: Option<u16>,
    lost: u16,
}

impl Depacketizer {
    /// Feed one RTP packet; returns a complete access unit when one closes.
    pub fn push(&mut self, pkt: &[u8]) -> Option<VideoAu> {
        if pkt.len() < 12 || pkt[0] >> 6 != 2 {
            return None;
        }
        let cc = (pkt[0] & 0x0f) as usize;
        let ext = pkt[0] & 0x10 != 0;
        let marker = pkt[1] & 0x80 != 0;
        let seq = u16::from_be_bytes([pkt[2], pkt[3]]);
        let ts = u32::from_be_bytes([pkt[4], pkt[5], pkt[6], pkt[7]]);
        let mut off = 12 + cc * 4;
        if ext {
            if pkt.len() < off + 4 {
                return None;
            }
            let words = u16::from_be_bytes([pkt[off + 2], pkt[off + 3]]) as usize;
            off += 4 + words * 4;
        }
        if pkt.len() <= off {
            return None;
        }
        if let Some(last) = self.last_seq {
            let gap = seq.wrapping_sub(last);
            if gap != 1 && gap < 0x8000 {
                self.lost = self.lost.saturating_add(gap - 1);
                self.fu = None; // a fragment with a hole is unusable
            }
        }
        self.last_seq = Some(seq);

        // A timestamp change without a marker still closes the previous AU.
        let mut finished = None;
        if let Some(prev) = self.au_ts {
            if prev != ts && !self.au.is_empty() {
                finished = self.finish();
            }
            // Backwards by more than half the space is a rollover, not reorder.
            if ts < prev && prev - ts > 0x8000_0000 {
                self.ts_wraps += 1;
            }
        }
        let base = *self.ts_base.get_or_insert(ts);
        self.au_ts_ext = (self.ts_wraps << 32)
            .wrapping_add(ts as u64)
            .wrapping_sub(base as u64);
        self.au_ts = Some(ts);

        let payload = &pkt[off..];
        let nal_type = payload[0] & 0x1f;
        match nal_type {
            1..=23 => self.push_nal(payload),
            24 => {
                // STAP-A
                let mut p = &payload[1..];
                while p.len() >= 2 {
                    let len = u16::from_be_bytes([p[0], p[1]]) as usize;
                    p = &p[2..];
                    if p.len() < len {
                        break;
                    }
                    self.push_nal(&p[..len]);
                    p = &p[len..];
                }
            }
            28 => {
                // FU-A
                if payload.len() < 2 {
                    return finished;
                }
                let fu_hdr = payload[1];
                let start = fu_hdr & 0x80 != 0;
                let end = fu_hdr & 0x40 != 0;
                if start {
                    let mut b = BytesMut::with_capacity(payload.len() + 1);
                    b.put_u8((payload[0] & 0xe0) | (fu_hdr & 0x1f));
                    b.extend_from_slice(&payload[2..]);
                    self.fu = Some(b);
                } else if let Some(b) = self.fu.as_mut() {
                    b.extend_from_slice(&payload[2..]);
                }
                if end {
                    if let Some(b) = self.fu.take() {
                        let nal = b.freeze();
                        self.push_nal(&nal);
                    }
                }
            }
            _ => {} // FU-B, STAP-B, MTAP: not produced by the encoders we target
        }
        if marker {
            if let Some(au) = self.finish() {
                return Some(au);
            }
        }
        finished
    }

    fn push_nal(&mut self, nal: &[u8]) {
        if nal.is_empty() {
            return;
        }
        if nal[0] & 0x1f == 5 {
            self.keyframe = true;
        }
        self.au.extend_from_slice(&START_CODE);
        self.au.extend_from_slice(nal);
    }

    fn finish(&mut self) -> Option<VideoAu> {
        if self.au.is_empty() {
            return None;
        }
        let data: Bytes = self.au.split().freeze();
        let au = VideoAu {
            data,
            keyframe: std::mem::take(&mut self.keyframe),
            capture_ts_us: self.au_ts_ext * 1_000_000 / RTP_CLOCK_HZ,
            input_loss: std::mem::take(&mut self.lost),
        };
        Some(au)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rtp(seq: u16, ts: u32, marker: bool, payload: &[u8]) -> Vec<u8> {
        let mut p = vec![0x80, if marker { 0x80 | 96 } else { 96 }];
        p.extend_from_slice(&seq.to_be_bytes());
        p.extend_from_slice(&ts.to_be_bytes());
        p.extend_from_slice(&[0, 0, 0, 1]);
        p.extend_from_slice(payload);
        p
    }

    #[test]
    fn single_nal_and_stap_a_form_one_au() {
        let mut d = Depacketizer::default();
        // STAP-A carrying SPS (7) and PPS (8), then IDR (5) with marker.
        let stap = [24u8, 0, 2, 0x67, 0xaa, 0, 2, 0x68, 0xbb];
        assert!(d.push(&rtp(1, 1000, false, &stap)).is_none());
        let au = d.push(&rtp(2, 1000, true, &[0x65, 1, 2, 3])).unwrap();
        assert!(au.keyframe);
        assert_eq!(
            &au.data[..],
            &[0, 0, 0, 1, 0x67, 0xaa, 0, 0, 0, 1, 0x68, 0xbb, 0, 0, 0, 1, 0x65, 1, 2, 3]
        );
    }

    #[test]
    fn fu_a_reassembles_and_loss_counts() {
        let mut d = Depacketizer::default();
        // NAL type 1 (non-IDR), nri 2 → indicator 0x41, header bits
        assert!(d.push(&rtp(10, 2000, false, &[0x5c, 0x81, 9, 9])).is_none()); // start
        assert!(d.push(&rtp(11, 2000, false, &[0x5c, 0x01, 8])).is_none());
        let au = d.push(&rtp(12, 2000, true, &[0x5c, 0x41, 7])).unwrap(); // end
        assert!(!au.keyframe);
        assert_eq!(&au.data[..], &[0, 0, 0, 1, 0x41, 9, 9, 8, 7]);
        // gap: seq 12 → 15
        let au2 = d.push(&rtp(15, 3000, true, &[0x41, 1])).unwrap();
        assert_eq!(au2.input_loss, 2);
    }

    #[test]
    fn timestamp_change_closes_au_without_marker() {
        let mut d = Depacketizer::default();
        assert!(d.push(&rtp(1, 100, false, &[0x41, 1])).is_none());
        let au = d.push(&rtp(2, 200, false, &[0x41, 2])).unwrap();
        assert_eq!(&au.data[..], &[0, 0, 0, 1, 0x41, 1]);
    }
}
