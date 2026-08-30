//! Closed-loop rate control inside the QoS ceiling (PLAN.md §1.3).
//!
//! A pure, deterministic controller stepped once per second with what the
//! transport and the pilot measured. It never exceeds the profile ceiling and
//! never goes below 25 % of it; it moves the *request* to the publisher
//! (`video-config`) and Seyd's own FEC rates.
//!
//! Rules, in priority order:
//!
//! 1. **FEC follows measured loss, burst-first.** Cellular loss arrives in
//!    bursts, and a block of 8 data chunks with `k = 1` (the 15 % default)
//!    dies to any burst of two. So the first response to *any* measurable
//!    loss (≥ 0.2 % over the last 5 s) is `k = 2` (25 %), then `k = 3` at
//!    ≥ 2 % (38 %) and `k = 4` at ≥ 5 % (50 %). Keyframe parity is at least
//!    delta + 10 (they are larger and losing one costs a GOP). FEC steps up
//!    immediately and steps down one level only after 5 clean seconds.
//! 2. **Raising FEC is paid for by the video rate.** The link constrains
//!    total bytes, so the bitrate request is scaled by
//!    `(1 + profile_fec) / (1 + fec)` — the on-wire budget stays what the
//!    profile promised.
//! 3. **Residual loss (frames lost *after* FEC, ≥ 1/s) without any latency
//!    signal is a loss problem, not a congestion problem:** step FEC up one
//!    level instead of cutting the rate (measured: cutting on residual loss
//!    alone sawtooths 25 % down / 10 % up forever on a lossy-but-uncongested
//!    link). Only when FEC is already at its top level does residual loss
//!    cut the bitrate.
//! 4. **Congestion: AIMD with latency gating.** Cut the bitrate 25 % when a
//!    delta frame was dropped for backlog or when `rtt − min_rtt` exceeds
//!    half the latency budget (the queue is building). Raise 10 % after 3
//!    consecutive clean seconds. Cuts are at most one per 2 s.
//! 5. **Emission hysteresis.** A bitrate decision is emitted at most every
//!    2 s and only for ≥ 10 % changes (encoders hate churn); FEC changes are
//!    emitted as soon as they happen.
//! 6. **At the floor and still congested for 5 s → `suggested_fps = 15`.**

use crate::Profile;
use std::collections::VecDeque;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Sample {
    pub rtt_ms: f64,
    pub min_rtt_ms: f64,
    /// Transport transmit rate over the last second.
    pub delivery_kbps: u64,
    pub lost_packets_delta: u64,
    pub frames_dropped_backlog_delta: u64,
    /// Chunk loss measured by pairing the agent's `chunks_sent` with the
    /// pilot's `chunksRx` over the last window, in percent. `None` until the
    /// pilot has reported.
    pub pilot_chunk_loss_pct: Option<f64>,
    /// Frames the pilot could not reconstruct even with parity.
    pub pilot_frames_incomplete_delta: u64,
    pub send_kbps: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    pub max_bitrate_kbps: u32,
    pub fec_delta_pct: u32,
    pub fec_key_pct: u32,
    pub suggested_fps: u32,
    /// Why this decision was taken: `loss`, `latency`, `backlog`, `residual`,
    /// `recover`, `fec-down`, `steady`.
    pub reason: &'static str,
    /// The bitrate moved (the publisher should be told).
    pub bitrate_changed: bool,
    /// The FEC rates moved (the sender applies them at the next frame).
    pub fec_changed: bool,
}

const FEC_LEVELS: [u32; 4] = [0, 25, 38, 50]; // index 0 = the profile's own value
const EMIT_INTERVAL_S: u64 = 2;
const CLEAN_SECONDS_TO_RAISE: u32 = 3;
const CLEAN_SECONDS_TO_LOWER_FEC: u32 = 5;
const FLOOR_FRACTION: f64 = 0.25;

#[derive(Debug, Clone)]
pub struct AbrController {
    profile: &'static Profile,
    tick: u64,
    /// Current bitrate target before the FEC budget scaling.
    target_kbps: f64,
    fec_level: usize,
    clean_seconds: u32,
    clean_seconds_fec: u32,
    congested_at_floor_seconds: u32,
    last_cut_tick: Option<u64>,
    last_emit_tick: Option<u64>,
    last_emitted_kbps: u32,
    loss_window: VecDeque<f64>,
    current: Decision,
}

impl AbrController {
    pub fn new(profile: &'static Profile) -> Self {
        let d = Decision {
            max_bitrate_kbps: profile.max_bitrate_kbps,
            fec_delta_pct: profile.fec_delta_pct,
            fec_key_pct: profile.fec_key_pct,
            suggested_fps: 0,
            reason: "steady",
            bitrate_changed: false,
            fec_changed: false,
        };
        Self {
            profile,
            tick: 0,
            target_kbps: profile.max_bitrate_kbps as f64,
            fec_level: 0,
            clean_seconds: 0,
            clean_seconds_fec: 0,
            congested_at_floor_seconds: 0,
            last_cut_tick: None,
            last_emit_tick: None,
            last_emitted_kbps: profile.max_bitrate_kbps,
            loss_window: VecDeque::new(),
            current: d,
        }
    }

    pub fn profile(&self) -> &'static Profile {
        self.profile
    }

    /// The decision in force (whatever was last emitted, plus live FEC).
    pub fn current(&self) -> Decision {
        self.current
    }

    fn fec_for_level(&self, level: usize) -> (u32, u32) {
        let delta = if level == 0 { self.profile.fec_delta_pct } else { FEC_LEVELS[level].max(self.profile.fec_delta_pct) };
        let key = self.profile.fec_key_pct.max(delta + 10).min(50);
        (delta, key)
    }

    fn floor_kbps(&self) -> f64 {
        (self.profile.max_bitrate_kbps as f64 * FLOOR_FRACTION).round()
    }

    /// Step once per second. Returns `Some` when something should be applied.
    pub fn step(&mut self, s: &Sample) -> Option<Decision> {
        self.tick += 1;
        let ceiling = self.profile.max_bitrate_kbps as f64;

        // ── loss over the last 5 s ─────────────────────────────────────────
        let loss_now = s.pilot_chunk_loss_pct.unwrap_or(0.0).max(0.0);
        self.loss_window.push_back(loss_now);
        if self.loss_window.len() > 5 {
            self.loss_window.pop_front();
        }
        let loss_avg = self.loss_window.iter().sum::<f64>() / self.loss_window.len() as f64;
        let loss_max = self.loss_window.iter().cloned().fold(0.0, f64::max);
        // Bursty loss shows as a high max against a low average; both count.
        let loss_signal = loss_avg.max(loss_max * 0.5);

        // ── FEC level from loss (up immediately, down after clean seconds) ──
        let wanted_level = if loss_signal >= 5.0 {
            3
        } else if loss_signal >= 2.0 {
            2
        } else if loss_signal >= 0.2 {
            1
        } else {
            0
        };
        let mut fec_changed = false;
        let mut reason: &'static str = "steady";
        if wanted_level > self.fec_level {
            self.fec_level = wanted_level;
            self.clean_seconds_fec = 0;
            fec_changed = true;
            reason = "loss";
        } else if wanted_level < self.fec_level {
            self.clean_seconds_fec += 1;
            if self.clean_seconds_fec >= CLEAN_SECONDS_TO_LOWER_FEC {
                self.fec_level -= 1;
                self.clean_seconds_fec = 0;
                fec_changed = true;
                reason = "fec-down";
            }
        } else {
            self.clean_seconds_fec = 0;
        }

        // ── congestion signals ─────────────────────────────────────────────
        let latency_inflated = s.min_rtt_ms > 0.0
            && (s.rtt_ms - s.min_rtt_ms) > self.profile.latency_budget_ms as f64 / 2.0;
        let backlog = s.frames_dropped_backlog_delta > 0;
        let residual = s.pilot_frames_incomplete_delta >= 1;
        // Residual loss with a healthy queue: more parity, not less video.
        if residual && !latency_inflated && !backlog && self.fec_level < FEC_LEVELS.len() - 1 {
            self.fec_level += 1;
            self.clean_seconds_fec = 0;
            fec_changed = true;
            reason = "residual";
        }
        let residual_cuts = residual && self.fec_level == FEC_LEVELS.len() - 1;
        let congested = latency_inflated || backlog || residual_cuts;

        if congested {
            self.clean_seconds = 0;
            let can_cut = self.last_cut_tick.map_or(true, |t| self.tick - t >= EMIT_INTERVAL_S);
            if can_cut && self.target_kbps > self.floor_kbps() {
                self.target_kbps = (self.target_kbps * 0.75).max(self.floor_kbps());
                self.last_cut_tick = Some(self.tick);
                reason = if backlog {
                    "backlog"
                } else if latency_inflated {
                    "latency"
                } else {
                    "residual"
                };
            }
            if self.target_kbps <= self.floor_kbps() {
                self.congested_at_floor_seconds += 1;
            }
        } else {
            self.congested_at_floor_seconds = 0;
            self.clean_seconds += 1;
            if self.clean_seconds >= CLEAN_SECONDS_TO_RAISE && self.target_kbps < ceiling {
                self.target_kbps = (self.target_kbps * 1.10).min(ceiling);
                self.clean_seconds = 0;
                if reason == "steady" {
                    reason = "recover";
                }
            }
        }

        // ── budget: pay for parity out of the video rate ───────────────────
        let (fec_delta, fec_key) = self.fec_for_level(self.fec_level);
        let budget_scale = (100.0 + self.profile.fec_delta_pct as f64) / (100.0 + fec_delta as f64);
        let bitrate = (self.target_kbps * budget_scale).clamp(self.floor_kbps(), ceiling).round() as u32;
        let suggested_fps = if self.congested_at_floor_seconds >= 5 { 15 } else { 0 };

        // ── emission hysteresis ────────────────────────────────────────────
        let delta_pct = (bitrate as f64 - self.last_emitted_kbps as f64).abs() / self.last_emitted_kbps.max(1) as f64;
        let emit_ok = self.last_emit_tick.map_or(true, |t| self.tick - t >= EMIT_INTERVAL_S);
        // ≥10 % moves, plus the exact ceiling/floor and any budget move caused
        // by a FEC change (that one is small by construction, ~8 %, but it is
        // what keeps the on-wire total inside the profile).
        let bitrate_changed = emit_ok
            && bitrate != self.last_emitted_kbps
            && (delta_pct >= 0.10 || fec_changed || bitrate == ceiling as u32 || bitrate == self.floor_kbps() as u32);
        if bitrate_changed {
            self.last_emitted_kbps = bitrate;
            self.last_emit_tick = Some(self.tick);
        }
        let fps_changed = suggested_fps != self.current.suggested_fps;

        if bitrate_changed || fec_changed || fps_changed {
            self.current = Decision {
                max_bitrate_kbps: self.last_emitted_kbps,
                fec_delta_pct: fec_delta,
                fec_key_pct: fec_key,
                suggested_fps,
                reason,
                bitrate_changed: bitrate_changed || fps_changed,
                fec_changed,
            };
            Some(self.current)
        } else {
            self.current.reason = reason;
            self.current.bitrate_changed = false;
            self.current.fec_changed = false;
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BALANCED;

    fn clean() -> Sample {
        Sample { rtt_ms: 25.0, min_rtt_ms: 24.0, delivery_kbps: 3000, send_kbps: 3000, pilot_chunk_loss_pct: Some(0.0), ..Default::default() }
    }

    fn run(c: &mut AbrController, samples: impl IntoIterator<Item = Sample>) -> Vec<Decision> {
        samples.into_iter().filter_map(|s| c.step(&s)).collect()
    }

    #[test]
    fn clean_link_stays_at_ceiling() {
        let mut c = AbrController::new(&BALANCED);
        let out = run(&mut c, std::iter::repeat(clean()).take(60));
        assert!(out.is_empty(), "no decisions on a clean link: {out:?}");
        assert_eq!(c.current().max_bitrate_kbps, 3000);
        assert_eq!(c.current().fec_delta_pct, 15);
    }

    #[test]
    fn sustained_loss_raises_fec_and_lowers_bitrate() {
        let mut c = AbrController::new(&BALANCED);
        let lossy = Sample { pilot_chunk_loss_pct: Some(1.0), ..clean() };
        let out = run(&mut c, std::iter::repeat(lossy).take(10));
        let first = out.first().expect("a decision");
        assert_eq!(first.fec_delta_pct, 25, "k=2 at any measurable loss");
        assert!(first.fec_key_pct >= 35);
        assert_eq!(first.reason, "loss");
        // 3000 × 1.15 / 1.25 = 2760
        assert_eq!(c.current().max_bitrate_kbps, 2760);
        // and it recovers after clean seconds
        let out = run(&mut c, std::iter::repeat(clean()).take(20));
        assert!(out.iter().any(|d| d.reason == "fec-down"));
        assert_eq!(c.current().fec_delta_pct, 15);
        assert_eq!(c.current().max_bitrate_kbps, 3000);
    }

    #[test]
    fn heavy_loss_reaches_top_fec_level() {
        let mut c = AbrController::new(&BALANCED);
        let bad = Sample { pilot_chunk_loss_pct: Some(6.0), ..clean() };
        run(&mut c, std::iter::repeat(bad).take(3));
        assert_eq!(c.current().fec_delta_pct, 50);
        assert_eq!(c.current().fec_key_pct, 50);
    }

    #[test]
    fn rtt_inflation_cuts_then_recovers() {
        let mut c = AbrController::new(&BALANCED);
        let inflated = Sample { rtt_ms: 90.0, min_rtt_ms: 24.0, ..clean() };
        let out = run(&mut c, std::iter::repeat(inflated).take(6));
        assert!(out.iter().any(|d| d.reason == "latency" && d.bitrate_changed));
        let low = c.current().max_bitrate_kbps;
        assert!(low < 2000, "cut at most once per 2 s: {low}");
        assert!(low >= 750, "never below the floor: {low}");
        let out = run(&mut c, std::iter::repeat(clean()).take(90));
        assert!(out.iter().any(|d| d.reason == "recover"));
        assert_eq!(c.current().max_bitrate_kbps, 3000);
    }

    #[test]
    fn floor_and_suggested_fps() {
        let mut c = AbrController::new(&BALANCED);
        let backlog = Sample { frames_dropped_backlog_delta: 2, ..clean() };
        run(&mut c, std::iter::repeat(backlog).take(30));
        assert_eq!(c.current().max_bitrate_kbps, 750);
        assert_eq!(c.current().suggested_fps, 15);
    }

    #[test]
    fn noisy_but_fine_link_does_not_oscillate() {
        let mut c = AbrController::new(&BALANCED);
        // rtt jitters ±15 ms around a 24 ms floor (budget/2 = 50 ms), loss flickers at 0.1 %
        let samples = (0..120).map(|i| Sample {
            rtt_ms: 24.0 + ((i * 7) % 30) as f64,
            pilot_chunk_loss_pct: Some(if i % 9 == 0 { 0.1 } else { 0.0 }),
            ..clean()
        });
        let out = run(&mut c, samples);
        assert!(out.is_empty(), "spurious decisions: {out:?}");
        assert_eq!(c.current().max_bitrate_kbps, 3000);
    }

    #[test]
    fn residual_loss_raises_fec_before_cutting_bitrate() {
        let mut c = AbrController::new(&BALANCED);
        let residual = Sample { pilot_frames_incomplete_delta: 1, ..clean() };
        let mut out = run(&mut c, [residual]);
        let d = out.pop().unwrap();
        assert_eq!((d.reason, d.fec_delta_pct), ("residual", 25));
        assert_eq!(d.max_bitrate_kbps, 2760, "budget scaling only, no 25 % cut");
        // keeps stepping FEC up to the top, then cuts
        run(&mut c, std::iter::repeat(residual).take(2));
        assert_eq!(c.current().fec_delta_pct, 50);
        run(&mut c, std::iter::repeat(residual).take(3));
        assert!(c.current().max_bitrate_kbps < 2300, "cut once FEC is maxed: {}", c.current().max_bitrate_kbps);
    }

    #[test]
    fn emission_needs_ten_percent() {
        let mut c = AbrController::new(&BALANCED);
        // one backlog second: cut 25 % → 2250 emitted; then clean: +10 % steps
        let mut out = run(&mut c, [Sample { frames_dropped_backlog_delta: 1, ..clean() }]);
        assert_eq!(out.pop().unwrap().max_bitrate_kbps, 2250);
        let out = run(&mut c, std::iter::repeat(clean()).take(4));
        assert!(out.iter().all(|d| d.max_bitrate_kbps >= 2475));
    }
}
