//! The drone's video datagrams → pictures the agent can take. The same rules
//! as `../h264rtp.py`, minus the RTP: a native host hands `seyd_core::Agent`
//! the Annex B access unit directly, with its keyframe flag and capture
//! timestamp, and the daemon's depacketizer is simply not in the path.
//!
//! * Whole picture or nothing: a frame with a missing datagram is dropped and
//!   reported as a loss. A lost *last* datagram leaves no index gap; the
//!   missing end-of-frame flag is the evidence, once the stream has shown it
//!   uses the flag.
//! * SPS/PPS are cached and put in front of every IDR; an IDR seen before any
//!   parameter sets is not forwarded (nothing can decode it) and another is
//!   requested.
//! * After a loss the delta frames keep flowing while a keyframe is requested
//!   and re-requested every 500 ms until one arrives (forward-on-loss, the
//!   behaviour the flights settled on).

use std::time::Instant;

pub const NAL_NON_IDR: u8 = 1;
pub const NAL_IDR: u8 = 5;
pub const NAL_SPS: u8 = 7;
pub const NAL_PPS: u8 = 8;

/// NAL units without start codes, in order; 3- and 4-byte start codes.
pub fn split_annexb(buf: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let find = |from: usize| -> Option<usize> {
        (from..buf.len().saturating_sub(2)).find(|&i| buf[i] == 0 && buf[i + 1] == 0 && buf[i + 2] == 1)
    };
    let Some(first) = find(0) else {
        if !buf.is_empty() {
            out.push(buf);
        }
        return out;
    };
    let mut start = first + 3;
    loop {
        match find(start) {
            Some(j) => {
                let mut end = j;
                if end > start && buf[end - 1] == 0 {
                    end -= 1;
                }
                if end > start {
                    out.push(&buf[start..end]);
                }
                start = j + 3;
            }
            None => {
                if start < buf.len() {
                    out.push(&buf[start..]);
                }
                return out;
            }
        }
    }
}

/// `avc1.PPCCLL` from an SPS.
pub fn sps_codec_string(sps: &[u8]) -> String {
    if sps.len() < 4 {
        return "?".into();
    }
    format!("avc1.{:02x}{:02x}{:02x}", sps[1], sps[2], sps[3])
}

/// One reassembled picture plus when its first and last datagrams arrived.
pub struct Picture {
    pub data: Vec<u8>,
    pub first_at: Instant,
    pub last_at: Instant,
}

#[derive(Default)]
pub struct AssemblerStats {
    pub frames: u64,
    pub lost_frames: u64,
    pub packets: u64,
}

pub struct FrameAssembler {
    frame_no: Option<u8>,
    expect_idx: u8,
    parts: Vec<u8>,
    first_at: Option<Instant>,
    torn: bool,
    uses_end_flag: bool,
    pub stats: AssemblerStats,
}

pub enum Assembled {
    Nothing,
    Picture(Picture),
    Lost,
}

impl FrameAssembler {
    pub fn new() -> Self {
        Self { frame_no: None, expect_idx: 0, parts: Vec::with_capacity(32 * 1024), first_at: None, torn: false, uses_end_flag: false, stats: AssemblerStats::default() }
    }

    /// Feed one datagram. At most one picture (or one loss) results per call
    /// in practice; a frame change closing the previous frame while the new
    /// datagram is itself a last one is the only case with two, and that one
    /// is reported as the previous frame's outcome (the new one is dropped
    /// as torn, since a one-datagram picture never occurs).
    pub fn push(&mut self, dgram: &[u8], now: Instant) -> Assembled {
        if dgram.len() < 3 {
            return Assembled::Nothing;
        }
        self.stats.packets += 1;
        let (frame_no, idx, last) = (dgram[0], dgram[1] & 0x7f, dgram[1] & 0x80 != 0);
        let mut result = Assembled::Nothing;
        if Some(frame_no) != self.frame_no {
            if !self.parts.is_empty() {
                result = self.close(true, now);
            }
            self.frame_no = Some(frame_no);
            self.expect_idx = 0;
            self.torn = idx != 0;
            self.parts.clear();
            self.first_at = Some(now);
        } else if idx != self.expect_idx {
            self.torn = true;
        }
        self.expect_idx = idx.wrapping_add(1);
        self.parts.extend_from_slice(&dgram[2..]);
        if last {
            self.uses_end_flag = true;
            let r = self.close(false, now);
            if matches!(result, Assembled::Nothing) {
                result = r;
            }
        }
        result
    }

    fn close(&mut self, missing_flag: bool, now: Instant) -> Assembled {
        let torn = self.torn || (missing_flag && self.uses_end_flag);
        let data = std::mem::take(&mut self.parts);
        let first_at = self.first_at.take().unwrap_or(now);
        self.torn = false;
        if torn {
            self.stats.lost_frames += 1;
            return Assembled::Lost;
        }
        self.stats.frames += 1;
        Assembled::Picture(Picture { data, first_at, last_at: now })
    }
}

#[derive(Default, Debug)]
pub struct RelayStats {
    pub frames: u64,
    pub idr: u64,
    pub dropped_before_first_idr: u64,
    pub param_only: u64,
    pub losses: u64,
    pub idr_without_params: u64,
    pub forwarded_unrepaired: u64,
}

/// What the relay hands the agent for one picture.
pub struct Outgoing {
    pub data: Vec<u8>,
    pub keyframe: bool,
}

pub struct Relay {
    sps: Option<Vec<u8>>,
    pps: Option<Vec<u8>>,
    awaiting_first_idr: bool,
    want_idr: bool,
    kf_requested_at: Option<Instant>,
    pub codec: Option<String>,
    pub last_idr_at: Option<Instant>,
    pub stats: RelayStats,
}

pub enum Verdict {
    Drop,
    Send(Outgoing),
}

const REREQUEST: std::time::Duration = std::time::Duration::from_millis(500);

impl Relay {
    pub fn new() -> Self {
        Self { sps: None, pps: None, awaiting_first_idr: true, want_idr: false, kf_requested_at: None, codec: None, last_idr_at: None, stats: RelayStats::default() }
    }

    /// A picture was lost on the drone link. Returns true when a keyframe
    /// should be requested now.
    pub fn mark_loss(&mut self, now: Instant) -> bool {
        self.stats.losses += 1;
        if self.awaiting_first_idr || self.want_idr {
            return false;
        }
        self.want_idr = true;
        self.kf_requested_at = Some(now);
        true
    }

    /// Returns the verdict and whether a keyframe should be requested now.
    pub fn push_picture(&mut self, data: &[u8], now: Instant) -> (Verdict, bool) {
        let mut vcl: Vec<&[u8]> = Vec::new();
        let mut idr = false;
        let mut request = false;
        for nal in split_annexb(data) {
            match nal[0] & 0x1f {
                NAL_SPS => {
                    if self.sps.as_deref() != Some(nal) {
                        self.sps = Some(nal.to_vec());
                        let codec = sps_codec_string(nal);
                        if self.codec.as_deref() != Some(&codec) {
                            tracing::info!(codec, profile_idc = nal[1], level_idc = nal[3], "stream profile from SPS");
                            self.codec = Some(codec);
                        }
                    }
                }
                NAL_PPS => self.pps = Some(nal.to_vec()),
                NAL_NON_IDR | NAL_IDR => {
                    idr |= nal[0] & 0x1f == NAL_IDR;
                    vcl.push(nal);
                }
                _ => {}
            }
        }
        if vcl.is_empty() {
            self.stats.param_only += 1;
            return (Verdict::Drop, false);
        }
        let mut out = Vec::with_capacity(data.len() + 64);
        if idr {
            let (Some(sps), Some(pps)) = (&self.sps, &self.pps) else {
                self.stats.idr_without_params += 1;
                self.kf_requested_at = Some(now);
                return (Verdict::Drop, true);
            };
            self.last_idr_at = Some(now);
            self.stats.idr += 1;
            self.awaiting_first_idr = false;
            self.want_idr = false;
            for n in [sps.as_slice(), pps.as_slice()] {
                out.extend_from_slice(&[0, 0, 0, 1]);
                out.extend_from_slice(n);
            }
        } else if self.awaiting_first_idr {
            self.stats.dropped_before_first_idr += 1;
            if self.kf_requested_at.map_or(true, |t| now - t >= REREQUEST) {
                self.kf_requested_at = Some(now);
                request = true;
            }
            return (Verdict::Drop, request);
        } else if self.want_idr {
            self.stats.forwarded_unrepaired += 1;
            if self.kf_requested_at.map_or(true, |t| now - t >= REREQUEST) {
                self.kf_requested_at = Some(now);
                request = true;
            }
        }
        for n in vcl {
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(n);
        }
        self.stats.frames += 1;
        (Verdict::Send(Outgoing { data: out, keyframe: idr }), request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dg(frame: u8, idx: u8, last: bool, body: &[u8]) -> Vec<u8> {
        let mut v = vec![frame, idx | if last { 0x80 } else { 0 }];
        v.extend_from_slice(body);
        v
    }

    #[test]
    fn assembler_end_flag_gap_and_lost_tail() {
        let t = Instant::now();
        let mut a = FrameAssembler::new();
        assert!(matches!(a.push(&dg(1, 0, false, b"ab"), t), Assembled::Nothing));
        match a.push(&dg(1, 1, true, b"cd"), t) {
            Assembled::Picture(p) => assert_eq!(p.data, b"abcd"),
            _ => panic!("expected a picture"),
        }
        // gap inside a frame
        a.push(&dg(2, 0, false, b"x"), t);
        assert!(matches!(a.push(&dg(2, 2, true, b"y"), t), Assembled::Lost));
        // lost last datagram: the flag is known, so a frame change counts as loss
        a.push(&dg(3, 0, false, b"x"), t);
        assert!(matches!(a.push(&dg(4, 0, false, b"y"), t), Assembled::Lost));
        assert_eq!(a.stats.lost_frames, 2);
    }

    const SPS: &[u8] = &[0, 0, 0, 1, 0x67, 0x42, 0x00, 0x1f, 0xaa];
    const PPS: &[u8] = &[0, 0, 0, 1, 0x68, 0xce, 0x38, 0x80];
    const IDR: &[u8] = &[0, 0, 0, 1, 0x65, 0x88, 0x84, 0x00];
    const P: &[u8] = &[0, 0, 0, 1, 0x41, 0x9a, 0x02, 0x03];

    #[test]
    fn relay_rules() {
        let t0 = Instant::now();
        let mut r = Relay::new();
        // deltas before any keyframe: dropped, keyframe asked for
        let (v, req) = r.push_picture(P, t0);
        assert!(matches!(v, Verdict::Drop) && req);
        // an IDR before its parameter sets is not forwarded
        let (v, req) = r.push_picture(IDR, t0);
        assert!(matches!(v, Verdict::Drop) && req);
        assert!(matches!(r.push_picture(SPS, t0).0, Verdict::Drop));
        assert!(matches!(r.push_picture(PPS, t0).0, Verdict::Drop));
        assert_eq!(r.codec.as_deref(), Some("avc1.42001f"));
        match r.push_picture(IDR, t0).0 {
            Verdict::Send(o) => {
                assert!(o.keyframe);
                assert_eq!(&o.data[..SPS.len()], SPS);
                assert_eq!(&o.data[SPS.len() + PPS.len()..], IDR);
            }
            _ => panic!("keyframe expected"),
        }
        // forward-on-loss: deltas keep flowing, re-request after 500 ms, repaired by the next IDR
        assert!(r.mark_loss(t0));
        assert!(!r.mark_loss(t0));
        let (v, req) = r.push_picture(P, t0 + std::time::Duration::from_millis(100));
        assert!(matches!(v, Verdict::Send(_)) && !req);
        let (v, req) = r.push_picture(P, t0 + std::time::Duration::from_millis(600));
        assert!(matches!(v, Verdict::Send(_)) && req);
        assert_eq!(r.stats.forwarded_unrepaired, 2);
        assert!(matches!(r.push_picture(IDR, t0 + std::time::Duration::from_secs(1)).0, Verdict::Send(_)));
        let (_, req) = r.push_picture(P, t0 + std::time::Duration::from_secs(2));
        assert!(!req);
    }
}
