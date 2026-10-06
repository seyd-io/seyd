//! Bare RTP over UDP — what `sim/video-source.sh` and most GStreamer/FFmpeg
//! pipelines emit. In-house depacketizer for H.264 (RFC 6184: single NAL,
//! STAP-A, FU-A) and H.265 (RFC 7798: single NAL, AP, FU); access units
//! delimited by the RTP marker bit or a timestamp change. Emits Annex B.
//! No decoding.
//!
//! The RTP framing — header, sequence, timestamp, marker — is identical for
//! both. Only the payload header differs, so the codec split is confined to
//! `push_payload` and the keyframe test in `push_nal`.

use super::{Codec, VideoAu};
use bytes::{BufMut, Bytes, BytesMut};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

const START_CODE: [u8; 4] = [0, 0, 0, 1];

pub async fn run(addr: String, codec: Codec, tx: mpsc::Sender<VideoAu>) {
    let sock = match UdpSocket::bind(&addr).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(%addr, error = %e, "rtp input: bind failed");
            return;
        }
    };
    tracing::info!(%addr, ?codec, "rtp input listening");
    let mut buf = vec![0u8; 65536];
    let mut dp = Depacketizer::new(codec);
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
    codec: Codec,
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
    pub fn new(codec: Codec) -> Self {
        Self {
            codec,
            ..Default::default()
        }
    }

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
        match self.codec {
            Codec::H264 => self.push_payload_h264(payload),
            Codec::H265 => self.push_payload_h265(payload),
            // Rejected in spawn_video: RFC 2435 needs JFIF reconstruction that
            // only the RTSP path (retina) performs.
            Codec::Mjpeg => {}
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
        let is_key = match self.codec {
            // H.264: nal_unit_type in the low 5 bits; 5 is an IDR slice.
            Codec::H264 => nal[0] & 0x1f == 5,
            // H.265: nal_unit_type is bits 6..1 of the first header byte, and
            // any IRAP picture (BLA_W_LP=16 … RSV_IRAP_VCL23=23) is a random
            // access point. Narrowing this to IDR alone would miss CRA
            // pictures, which several camera encoders emit instead.
            Codec::H265 => nal.len() >= 2 && matches!((nal[0] >> 1) & 0x3f, 16..=23),
            Codec::Mjpeg => true,
        };
        if is_key {
            self.keyframe = true;
        }
        self.au.extend_from_slice(&START_CODE);
        self.au.extend_from_slice(nal);
    }

    /// RFC 6184: single NAL unit, STAP-A aggregation, FU-A fragmentation.
    fn push_payload_h264(&mut self, payload: &[u8]) {
        match payload[0] & 0x1f {
            1..=23 => self.push_nal(payload),
            24 => {
                // STAP-A: 1-byte header, then repeated 2-byte length + NAL.
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
                // FU-A: rebuild the 1-byte NAL header from the indicator's
                // top 3 bits and the FU header's type.
                if payload.len() < 2 {
                    return;
                }
                let fu = payload[1];
                if fu & 0x80 != 0 {
                    let mut b = BytesMut::with_capacity(payload.len() + 1);
                    b.put_u8((payload[0] & 0xe0) | (fu & 0x1f));
                    b.extend_from_slice(&payload[2..]);
                    self.fu = Some(b);
                } else if let Some(b) = self.fu.as_mut() {
                    b.extend_from_slice(&payload[2..]);
                }
                if fu & 0x40 != 0 {
                    if let Some(b) = self.fu.take() {
                        self.push_nal(&b.freeze());
                    }
                }
            }
            _ => {} // FU-B, STAP-B, MTAP: not produced by the encoders we target
        }
    }

    /// RFC 7798: the header is two bytes, and the packet type sits where
    /// H.264 keeps its NAL type — hence 48 (AP) and 49 (FU) rather than 24
    /// and 28. `sprop-max-don-diff` is assumed 0, so there is no DONL field;
    /// no camera encoder we target sets it.
    fn push_payload_h265(&mut self, payload: &[u8]) {
        if payload.len() < 2 {
            return;
        }
        match (payload[0] >> 1) & 0x3f {
            0..=47 => self.push_nal(payload),
            48 => {
                // AP: 2-byte header, then repeated 2-byte length + NAL.
                let mut p = &payload[2..];
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
            49 => {
                // FU: 2-byte header + 1-byte FU header. Rebuild the original
                // 2-byte NAL header by putting the FU type back in place.
                if payload.len() < 3 {
                    return;
                }
                let fu = payload[2];
                if fu & 0x80 != 0 {
                    let mut b = BytesMut::with_capacity(payload.len());
                    b.put_u8((payload[0] & 0x81) | ((fu & 0x3f) << 1));
                    b.put_u8(payload[1]);
                    b.extend_from_slice(&payload[3..]);
                    self.fu = Some(b);
                } else if let Some(b) = self.fu.as_mut() {
                    b.extend_from_slice(&payload[3..]);
                }
                if fu & 0x40 != 0 {
                    if let Some(b) = self.fu.take() {
                        self.push_nal(&b.freeze());
                    }
                }
            }
            _ => {} // PACI (50) and reserved: not produced by the encoders we target
        }
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
        let mut d = Depacketizer::new(Codec::H264);
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
        let mut d = Depacketizer::new(Codec::H264);
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
        let mut d = Depacketizer::new(Codec::H265);
        assert!(d.push(&rtp(1, 100, false, &[0x41, 1])).is_none());
        let au = d.push(&rtp(2, 200, false, &[0x41, 2])).unwrap();
        assert_eq!(&au.data[..], &[0, 0, 0, 1, 0x41, 1]);
    }
}

// ── H.265 (RFC 7798) ────────────────────────────────────────────────────────
#[cfg(test)]
mod h265_tests {
    use super::*;

    fn rtp(seq: u16, ts: u32, marker: bool, payload: &[u8]) -> Vec<u8> {
        let mut p = vec![0x80, if marker { 0x80 | 96 } else { 96 }];
        p.extend_from_slice(&seq.to_be_bytes());
        p.extend_from_slice(&ts.to_be_bytes());
        p.extend_from_slice(&[0, 0, 0, 1]);
        p.extend_from_slice(payload);
        p
    }

    /// Two-byte H.265 NAL header for a given nal_unit_type, layer 0, tid 1.
    fn hdr(nal_type: u8) -> [u8; 2] {
        [nal_type << 1, 0x01]
    }

    #[test]
    fn aggregation_packet_then_idr_forms_one_keyframe_au() {
        let mut d = Depacketizer::new(Codec::H265);
        // AP (type 48) carrying VPS(32), SPS(33), PPS(34).
        let mut ap = vec![48u8 << 1, 0x01];
        for t in [32u8, 33, 34] {
            ap.extend_from_slice(&[0, 3]);
            ap.extend_from_slice(&hdr(t));
            ap.push(0xaa);
        }
        assert!(d.push(&rtp(1, 1000, false, &ap)).is_none());

        // IDR_W_RADL is type 19 — an IRAP, so a keyframe.
        let mut idr = hdr(19).to_vec();
        idr.extend_from_slice(&[1, 2, 3]);
        let au = d.push(&rtp(2, 1000, true, &idr)).unwrap();
        assert!(
            au.keyframe,
            "IDR_W_RADL must mark the access unit as a keyframe"
        );
        assert_eq!(
            &au.data[..],
            &[
                0, 0, 0, 1, 64, 1, 0xaa, // VPS
                0, 0, 0, 1, 66, 1, 0xaa, // SPS
                0, 0, 0, 1, 68, 1, 0xaa, // PPS
                0, 0, 0, 1, 38, 1, 1, 2, 3, // IDR
            ]
        );
    }

    #[test]
    fn cra_counts_as_a_keyframe_but_a_trailing_picture_does_not() {
        // Several camera encoders open a GOP with CRA (21) rather than IDR;
        // treating only IDR as a random access point would strand them.
        let mut d = Depacketizer::new(Codec::H265);
        let mut cra = hdr(21).to_vec();
        cra.push(9);
        assert!(d.push(&rtp(1, 100, true, &cra)).unwrap().keyframe);

        let mut trail = hdr(1).to_vec(); // TRAIL_R
        trail.push(9);
        assert!(!d.push(&rtp(2, 200, true, &trail)).unwrap().keyframe);
    }

    #[test]
    fn fragmentation_unit_rebuilds_the_two_byte_header() {
        let mut d = Depacketizer::new(Codec::H265);
        // FU (49) carrying a fragmented IDR (19): payload header, FU header, data.
        let fu = |s: bool, e: bool, data: &[u8]| {
            let mut p = vec![49u8 << 1, 0x01];
            p.push((if s { 0x80 } else { 0 }) | (if e { 0x40 } else { 0 }) | 19);
            p.extend_from_slice(data);
            p
        };
        assert!(d
            .push(&rtp(10, 500, false, &fu(true, false, &[7, 7])))
            .is_none());
        assert!(d
            .push(&rtp(11, 500, false, &fu(false, false, &[8])))
            .is_none());
        let au = d.push(&rtp(12, 500, true, &fu(false, true, &[9]))).unwrap();
        assert!(au.keyframe);
        // Reassembled as one NAL with the original IDR header restored.
        assert_eq!(&au.data[..], &[0, 0, 0, 1, 38, 1, 7, 7, 8, 9]);
    }

    #[test]
    fn a_hole_in_a_fragment_discards_it_rather_than_emitting_a_torn_nal() {
        let mut d = Depacketizer::new(Codec::H265);
        let mut start = vec![49u8 << 1, 0x01, 0x80 | 19];
        start.extend_from_slice(&[1, 2]);
        assert!(d.push(&rtp(1, 900, false, &start)).is_none());
        // seq jumps 1 → 3: the fragment is unusable and must be dropped.
        let mut end = vec![49u8 << 1, 0x01, 0x40 | 19];
        end.push(3);
        assert!(d.push(&rtp(3, 900, true, &end)).is_none());
    }

    #[test]
    fn codec_strings_map_to_the_right_framing() {
        assert_eq!(
            Codec::from_codec_string("avc1.42001f").unwrap(),
            Codec::H264
        );
        assert_eq!(
            Codec::from_codec_string("hev1.1.6.L93.B0").unwrap(),
            Codec::H265
        );
        assert_eq!(
            Codec::from_codec_string("hvc1.1.6.L93.B0").unwrap(),
            Codec::H265
        );
        assert_eq!(Codec::from_codec_string("H265").unwrap(), Codec::H265);
        assert!(Codec::from_codec_string("vp09.00.10.08").is_err());
    }
}
