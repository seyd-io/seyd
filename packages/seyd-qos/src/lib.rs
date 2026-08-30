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

pub mod abr;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OnLoss {
    Continue,
    FreezeUntilIdr,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Profile {
    pub name: &'static str,
    // ── targets the publisher must honour ───────────────────────────────
    pub max_bitrate_kbps: u32,
    pub latency_budget_ms: u32,
    pub max_gop_ms: u32,
    // ── Seyd's own transport policy ──────────────────────────────────────
    pub fec_delta_pct: u32,
    pub fec_key_pct: u32,
    /// Drop a delta frame if the send backlog exceeds this many frame-times.
    pub backlog_drop_frames: u32,
    pub pilot_deadline_delta_ms: u32,
    pub pilot_deadline_key_ms: u32,
    pub on_loss: OnLoss,
}

pub const LATENCY: Profile = Profile {
    name: "latency",
    max_bitrate_kbps: 1500,
    latency_budget_ms: 100,
    max_gop_ms: 1000,
    fec_delta_pct: 25,
    fec_key_pct: 50,
    backlog_drop_frames: 1,
    pilot_deadline_delta_ms: 20,
    pilot_deadline_key_ms: 40,
    on_loss: OnLoss::Continue,
};

pub const BALANCED: Profile = Profile {
    name: "balanced",
    max_bitrate_kbps: 3000,
    latency_budget_ms: 100,
    max_gop_ms: 1000,
    fec_delta_pct: 15,
    fec_key_pct: 30,
    backlog_drop_frames: 2,
    pilot_deadline_delta_ms: 30,
    pilot_deadline_key_ms: 60,
    on_loss: OnLoss::Continue,
};

pub const QUALITY: Profile = Profile {
    name: "quality",
    max_bitrate_kbps: 6000,
    latency_budget_ms: 200,
    max_gop_ms: 2000,
    fec_delta_pct: 8,
    fec_key_pct: 15,
    backlog_drop_frames: 3,
    pilot_deadline_delta_ms: 50,
    pilot_deadline_key_ms: 100,
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
        let bytes_per_frame = self.max_bitrate_kbps as f64 * 1000.0 / 8.0 / fps.max(1) as f64;
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
            "suggestedFps": 0,
            "reason": reason,
        })
    }

    /// What the pilot needs: close-out deadlines and loss policy
    /// (`welcome.qos` / `qos-ack.qos` in docs/protocol/control-stream.md).
    pub fn pilot_config(&self) -> serde_json::Value {
        serde_json::json!({
            "profile": self.name,
            "deadline_delta_ms": self.pilot_deadline_delta_ms,
            "deadline_key_ms": self.pilot_deadline_key_ms,
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
    fn pilot_config_serialises_on_loss_kebab() {
        let v = QUALITY.pilot_config();
        assert_eq!(v["on_loss"], "freeze-until-idr");
        assert_eq!(v["deadline_key_ms"], 100);
    }
}
