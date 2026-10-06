// Copyright 2026 Anton Gravestam
// SPDX-License-Identifier: Apache-2.0
//! QoS profiles — the latency/quality trade-off as a named choice.
//!
//! The link constrains *total bytes on the wire*, so a profile is one budget
//! split three ways: pixels, redundancy, headroom. The boundary from SPEC.md:
//! Seyd owns the transport-observable targets (bitrate ceiling, latency budget,
//! max GOP) and its own policy (FEC rates, backlog drop threshold, the pilot's
//! close-out deadlines). The publisher owns resolution, preset, VBV and the
//! actual GOP. **There is no resolution here, and there must never be.**
//!
//! The closed-loop ABR controller that moves the bitrate *inside* the ceiling
//! is `abr::AbrController`; the profile is its bound.
//!
//! Where a publisher offers several encodings of the same picture,
//! `simulcast::LayerSelector` picks which one those bits buy (ADR 0008). Note
//! that it names layers and the bitrate each needs — never their resolution, so
//! the boundary above still holds.

pub mod abr;
pub mod simulcast;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OnLoss {
    Continue,
    FreezeUntilIdr,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Profile {
    /// The profile's name: `latency`, `balanced` or `quality`.
    pub name: &'static str,
    // ── targets the publisher must honour ───────────────────────────────
    /// The ceiling on the publisher's video bitrate; the closed-loop controller
    /// moves the actual request between 25 % of this and this.
    pub max_bitrate_kbps: u32,
    /// How much end-to-end delay the profile is prepared to spend, told to the
    /// publisher so it can size its own buffers (VBV, lookahead).
    pub latency_budget_ms: u32,
    /// The longest the publisher may go without a full recovery point — an
    /// IDR, or a completed intra-refresh sweep. A *ceiling*, and a safety net:
    /// Seyd asks for recovery points when it needs them (`recovery-request`),
    /// so the periodic one only has to catch a publisher that ignores it
    /// (ADR 0009). It is long on purpose: a periodic IDR is several times a
    /// delta frame, and measured on the demo camera it arrived ~22 ms late and
    /// made every GOP's first two intervals 60 ms then 18 ms; at 10 s GOPs
    /// arrival judder fell from 3.7 to 0.9 ms mean and the stream lost 43 % of
    /// its bitrate, the IDRs' share. A joining pilot never waits for it: the
    /// agent asks for an IDR on `hello`.
    pub max_gop_ms: u32,
    /// Ask the publisher to refresh the picture gradually — a strip of
    /// intra-coded blocks per frame sweeping the picture within `max_gop_ms` —
    /// rather than with periodic IDRs, where its encoder can (x264, NVENC,
    /// Jetson; not the ONVIF cameras surveyed). Removes the keyframe burst
    /// entirely; measured on the sim through a shaped link as g2g p95
    /// 148 → 48 ms (docs/latency-sources.md §11).
    pub prefer_intra_refresh: bool,
    // ── Seyd's own transport policy ──────────────────────────────────────
    /// Reed-Solomon parity added to a delta frame's block, as a percentage of
    /// its data chunks, on a clean link; the controller raises it under loss.
    pub fec_delta_pct: u32,
    /// Parity for keyframes, which are larger and cost a GOP if torn.
    pub fec_key_pct: u32,
    /// Drop a delta frame if the send backlog exceeds this many frame-times.
    pub backlog_drop_frames: u32,
    /// How long the pilot waits for a delta frame's missing chunks before
    /// closing it out.
    pub pilot_deadline_delta_ms: u32,
    /// The same wait for a keyframe.
    pub pilot_deadline_key_ms: u32,
    /// How long the pilot holds a decoded frame past its capture slot before
    /// painting it (ADR 0005). This is the profile's judder-versus-latency
    /// call: a keyframe is ~8x a delta frame and is produced in one frame slot,
    /// so without a budget here the picture hitches once per GOP. It is a
    /// bound, not an average — a frame never waits longer than this past its
    /// own arrival — so it is the latency the profile is prepared to spend.
    /// 0 disables pacing and restores decode-on-arrival.
    pub pilot_presentation_delay_ms: u32,
    /// How long the sender waits for a recovery point after asking for one
    /// before it resumes sending delta frames regardless.
    ///
    /// A publisher that recovers with intra-refresh or an LTR reference never
    /// produces a keyframe, so waiting for one latches the stream silent
    /// forever. Roughly one refresh cycle: long enough that a real keyframe
    /// arrives first when the publisher sends IDRs, short enough that a stall
    /// is not visible as a freeze.
    pub recovery_grace_ms: u32,
    /// What the pilot does when a frame cannot be repaired: keep decoding
    /// (`continue`, and ask for a recovery point) or freeze until the next IDR.
    pub on_loss: OnLoss,
}

pub const LATENCY: Profile = Profile {
    name: "latency",
    max_bitrate_kbps: 1500,
    latency_budget_ms: 100,
    max_gop_ms: 10_000,
    prefer_intra_refresh: true,
    fec_delta_pct: 25,
    fec_key_pct: 50,
    backlog_drop_frames: 1,
    pilot_deadline_delta_ms: 20,
    pilot_deadline_key_ms: 40,
    // Half the default: still covers the measured keyframe excess (15–80 ms),
    // and this profile spends latency nowhere it does not have to.
    pilot_presentation_delay_ms: 50,
    recovery_grace_ms: 700,
    on_loss: OnLoss::Continue,
};

pub const BALANCED: Profile = Profile {
    name: "balanced",
    max_bitrate_kbps: 3000,
    latency_budget_ms: 100,
    max_gop_ms: 10_000,
    prefer_intra_refresh: true,
    fec_delta_pct: 15,
    fec_key_pct: 30,
    backlog_drop_frames: 2,
    pilot_deadline_delta_ms: 30,
    pilot_deadline_key_ms: 60,
    // Measured on the demo camera: judder p95 77 ms → 40 ms, and hitches over
    // 120 ms fall from 61 to 7 per 40 s. See ADR 0005 for the full table.
    pilot_presentation_delay_ms: 100,
    recovery_grace_ms: 900,
    on_loss: OnLoss::Continue,
};

pub const QUALITY: Profile = Profile {
    name: "quality",
    max_bitrate_kbps: 6000,
    latency_budget_ms: 200,
    // Shorter than the other profiles: this one freezes on loss until an IDR,
    // so the safety net has to be closer for a publisher that ignores requests.
    max_gop_ms: 4000,
    prefer_intra_refresh: true,
    fec_delta_pct: 8,
    fec_key_pct: 15,
    backlog_drop_frames: 3,
    pilot_deadline_delta_ms: 50,
    pilot_deadline_key_ms: 100,
    // This profile already spends 200 ms of latency budget and a 2 s GOP, so
    // the extra 50 ms buys the smoothest picture available: judder p95 15 ms.
    pilot_presentation_delay_ms: 150,
    recovery_grace_ms: 1400,
    on_loss: OnLoss::FreezeUntilIdr,
};

pub const PROFILES: [&Profile; 3] = [&LATENCY, &BALANCED, &QUALITY];
pub const DEFAULT: &Profile = &BALANCED;

/// Look up a profile by name (case-insensitive). `None` for unknown names —
/// callers decide whether to fall back to `DEFAULT` or reject.
pub fn get(name: &str) -> Option<&'static Profile> {
    let n = name.trim().to_ascii_lowercase();
    PROFILES.iter().copied().find(|p| p.name == n)
}

impl Profile {
    /// Backlog above which a delta frame is dropped before any of it is sent.
    /// A byte budget rather than "queue non-empty": the transport's pending
    /// queue also grows merely because the pacer is spacing packets out.
    pub fn drop_threshold_bytes(&self, fps: u32) -> usize {
        self.drop_threshold_bytes_at(self.max_bitrate_kbps, fps)
    }

    /// The same budget at a ceiling other than the profile's own — a
    /// publisher that tops out lower has smaller frames, and a backlog of
    /// "a few frames" is correspondingly fewer bytes.
    pub fn drop_threshold_bytes_at(&self, ceiling_kbps: u32, fps: u32) -> usize {
        let bytes_per_frame = ceiling_kbps as f64 * 1000.0 / 8.0 / fps.max(1) as f64;
        (self.backlog_drop_frames as f64 * bytes_per_frame) as usize
    }

    /// The half of the profile the video publisher is responsible for, as the
    /// `video-config` publisher-control message (docs/protocol/seydd.md).
    pub fn publisher_config(&self, channel: u8, reason: &str) -> serde_json::Value {
        serde_json::json!({
            "type": "video-config",
            "channel": channel,
            "profile": self.name,
            "maxBitrateKbps": self.max_bitrate_kbps,
            "latencyBudgetMs": self.latency_budget_ms,
            "maxGopMs": self.max_gop_ms,
            "preferIntraRefresh": self.prefer_intra_refresh,
            "suggestedFps": 0,
            "reason": reason,
        })
    }

    /// What the pilot needs: close-out deadlines, presentation delay and loss
    /// policy (`welcome.qos` / `qos-ack.qos` in docs/protocol/control-stream.md).
    pub fn pilot_config(&self) -> serde_json::Value {
        serde_json::json!({
            "profile": self.name,
            "deadline_delta_ms": self.pilot_deadline_delta_ms,
            "deadline_key_ms": self.pilot_deadline_key_ms,
            "presentation_delay_ms": self.pilot_presentation_delay_ms,
            "on_loss": self.on_loss,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup() {
        assert_eq!(get("Latency").unwrap().name, "latency");
        assert!(get("turbo").is_none());
    }

    #[test]
    fn drop_threshold_matches_python() {
        // balanced, 30 fps: 2 frames × 12 500 B
        assert_eq!(BALANCED.drop_threshold_bytes(30), 25_000);
        assert_eq!(LATENCY.drop_threshold_bytes(25), 7_500);
    }

    #[test]
    fn keyframes_are_on_demand_with_a_long_safety_net() {
        // ADR 0009: the periodic keyframe is a safety net for a publisher that
        // ignores recovery requests, not the recovery mechanism, so it is long
        // — and intra refresh is asked for wherever the encoder has it.
        for p in PROFILES {
            assert!(p.max_gop_ms >= 4000, "{}: {} ms", p.name, p.max_gop_ms);
            assert!(p.prefer_intra_refresh);
        }
        // The profile that freezes on loss until an IDR keeps its net closer.
        let (q, b) = (get("quality").unwrap(), get("balanced").unwrap());
        assert!(
            q.max_gop_ms < b.max_gop_ms,
            "{} vs {}",
            q.max_gop_ms,
            b.max_gop_ms
        );
        let v = BALANCED.publisher_config(1, "profile");
        assert_eq!(v["maxGopMs"], 10_000);
        assert_eq!(v["preferIntraRefresh"], true);
    }

    #[test]
    fn pilot_config_serialises_on_loss_kebab() {
        let v = QUALITY.pilot_config();
        assert_eq!(v["on_loss"], "freeze-until-idr");
        assert_eq!(v["deadline_key_ms"], 100);
    }

    #[test]
    fn presentation_delay_rises_with_the_profile_latency_budget() {
        // The pacing budget (ADR 0005) is a latency spend, so it must be
        // ordered the same way as every other latency knob in the profiles.
        let v = BALANCED.pilot_config();
        assert_eq!(v["presentation_delay_ms"], 100);
        let delays: Vec<u32> = ["latency", "balanced", "quality"]
            .iter()
            .map(|n| get(n).unwrap().pilot_presentation_delay_ms)
            .collect();
        assert!(delays.windows(2).all(|w| w[0] < w[1]), "{delays:?}");
        // It must stay inside the budget the publisher is asked to hit.
        for p in PROFILES {
            assert!(
                p.pilot_presentation_delay_ms <= p.latency_budget_ms,
                "{} spends {} ms pacing of a {} ms budget",
                p.name,
                p.pilot_presentation_delay_ms,
                p.latency_budget_ms
            );
        }
    }
}
