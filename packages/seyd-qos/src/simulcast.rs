// Copyright 2026 Anton Gravestam
// SPDX-License-Identifier: Apache-2.0
//! Which encoding to relay, given how many bits the link will carry.
//!
//! The `AbrController` decides *how many bits*; this decides *which of the
//! publisher's encodings fits them* (ADR 0008). The two are deliberately
//! separate: a robot that publishes one layer runs the controller unchanged and
//! never consults this module.
//!
//! Pure and allocation-light: no I/O, no clock. The caller steps it once per
//! second with the controller's current target, exactly as it steps the ABR.

use serde::{Deserialize, Serialize};

/// How far above a layer's activation point the target must sit before we climb
/// onto it. A layer change costs the pilot a decoder reconfigure, so buying the
/// rung outright beats oscillating around its edge.
const UP_MARGIN_PCT: u32 = 25;

/// Seconds the target must stay *continuously* affordable before climbing.
///
/// A dwell timer alone is not enough. Measured against the real ABR under
/// sustained loss, the controller sawtooths — cutting on congestion, recovering
/// 10 % per 5 clean seconds — so the target crosses a rung's threshold every
/// cycle. Sampling "is it affordable now?" once a dwell expires climbs on that
/// transient peak and drops again seconds later, costing the pilot a decoder
/// reconfigure each way.
///
/// Downward moves are immediate: sitting a rung too high under congestion is
/// what tears frames.
const UP_HOLD_S: u32 = 10;

/// How far the target must beat a rung's *proven insufficient* level to be worth
/// trying again.
const RETRY_MARGIN_PCT: u32 = 10;

/// Seconds without a demotion after which what we learned is forgotten and the
/// ladder probes upward again.
///
/// Probing has to happen: a link that genuinely recovers must get its picture
/// back, and the only way to discover that is to try. But a failed probe costs
/// two switches, so it is deliberately rare.
const FORGIVE_S: u32 = 600;

/// One independently encoded stream of a video channel's picture.
///
/// `activate_above_kbps` is the ABR target at or above which this layer is the
/// right choice. The lowest layer is the base and is always eligible, whatever
/// its value, so a link below every activation point still gets video.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoLayer {
    pub id: u8,
    pub name: String,
    #[serde(default)]
    pub activate_above_kbps: u32,
}

/// A change of layer, and why. Returned only when the layer actually moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub layer: u8,
    pub reason: &'static str,
}

/// Stateful chooser over a fixed set of layers.
///
/// Selection and the ABR form a closed loop: relaying a higher rung puts more
/// bytes on the wire, so on a constrained link the controller cuts, which
/// demotes the rung, which frees the link and lets the target recover — and the
/// cycle repeats. Measured on the demo camera under 12 % loss it ran every
/// 10-40 s indefinitely. Neither a hold nor a backoff damps it on its own,
/// because the cycle is *caused by the switch*, so the selector remembers the
/// target at which each rung failed and refuses to climb back for less.
#[derive(Debug, Clone)]
pub struct LayerSelector {
    /// Sorted by `activate_above_kbps` ascending; index 0 is the base layer.
    layers: Vec<VideoLayer>,
    current: usize,
    /// Consecutive seconds the next rung up has been affordable.
    above_s: u32,
    /// Per rung: the highest target that turned out not to sustain it. 0 until
    /// a climb onto that rung has actually failed.
    insufficient: Vec<u32>,
    /// Per rung: the target at which we last climbed onto it, so a demotion
    /// knows what to write into `insufficient`. 0 means "never climbed onto",
    /// which includes the rung the selector was constructed on — the ABR simply
    /// starts optimistic there, so failing to hold it proves nothing about the
    /// link and must not be learned from.
    climbed_at: Vec<u32>,
    /// Seconds since the last demotion, for forgiving what we learned.
    settled_s: u32,
}

impl LayerSelector {
    /// Build a selector over `layers`, settled at whichever layer `target_kbps`
    /// affords. Starting where the link already is avoids a gratuitous switch in
    /// the first seconds of a session.
    ///
    /// Returns `None` if no layer was declared; a single layer is fine and makes
    /// `step` a no-op forever.
    pub fn new(mut layers: Vec<VideoLayer>, target_kbps: u32) -> Option<Self> {
        if layers.is_empty() {
            return None;
        }
        layers.sort_by_key(|l| l.activate_above_kbps);
        let current = Self::affordable(&layers, target_kbps);
        let n = layers.len();
        Some(Self {
            layers,
            current,
            above_s: 0,
            insufficient: vec![0; n],
            climbed_at: vec![0; n],
            settled_s: 0,
        })
    }

    /// Highest layer whose activation point the target meets. Index 0 always
    /// qualifies, so this never fails to pick something.
    fn affordable(layers: &[VideoLayer], target_kbps: u32) -> usize {
        layers
            .iter()
            .rposition(|l| target_kbps >= l.activate_above_kbps)
            .unwrap_or(0)
    }

    pub fn current(&self) -> u8 {
        self.layers[self.current].id
    }

    pub fn current_name(&self) -> &str {
        &self.layers[self.current].name
    }

    pub fn layers(&self) -> &[VideoLayer] {
        &self.layers
    }

    /// What the target must reach to climb onto `rung`: its declared activation
    /// point plus a margin, and never less than a level already proven not to
    /// hold it.
    fn threshold(&self, rung: usize) -> u32 {
        let declared = self.layers[rung]
            .activate_above_kbps
            .saturating_mul(100 + UP_MARGIN_PCT)
            / 100;
        let learned = self.insufficient[rung].saturating_mul(100 + RETRY_MARGIN_PCT) / 100;
        declared.max(learned)
    }

    /// Step once per second with the ABR's current target. `Some` when the layer
    /// changed and the host should switch.
    pub fn step(&mut self, target_kbps: u32) -> Option<Selection> {
        if self.layers.len() < 2 {
            return None;
        }
        self.settled_s = self.settled_s.saturating_add(1);
        if self.settled_s >= FORGIVE_S {
            self.insufficient.iter_mut().for_each(|v| *v = 0);
            self.settled_s = 0;
        }

        // Down: immediate. The current layer is unaffordable the moment the
        // target falls below what it was declared to need.
        if target_kbps < self.layers[self.current].activate_above_kbps {
            self.above_s = 0;
            let to = Self::affordable(&self.layers, target_kbps);
            if to != self.current {
                return Some(self.switch(to, "down"));
            }
            return None;
        }

        // Up: onto the next rung only, once the target has cleared its threshold
        // for a whole window.
        let next = self.current + 1;
        if next >= self.layers.len() {
            self.above_s = 0;
            return None;
        }
        if target_kbps < self.threshold(next) {
            self.above_s = 0;
            return None;
        }
        self.above_s = self.above_s.saturating_add(1);
        if self.above_s >= UP_HOLD_S {
            self.climbed_at[next] = target_kbps;
            return Some(self.switch(next, "up"));
        }
        None
    }

    fn switch(&mut self, to: usize, reason: &'static str) -> Selection {
        if to < self.current {
            // Coming back down means the target we climbed at was not enough to
            // sustain this rung. Remember it, so the next climb needs better.
            // Only if we actually climbed: a demotion off the starting rung says
            // nothing, and treating the ABR's opening ceiling as "proven
            // insufficient" would bar that rung for good.
            let failed_at = self.climbed_at[self.current];
            if failed_at > 0 {
                self.insufficient[self.current] = self.insufficient[self.current].max(failed_at);
            }
            self.settled_s = 0;
        }
        self.current = to;
        self.above_s = 0;
        Selection {
            layer: self.layers[to].id,
            reason,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ladder() -> Vec<VideoLayer> {
        vec![
            VideoLayer {
                id: 0,
                name: "low".into(),
                activate_above_kbps: 0,
            },
            VideoLayer {
                id: 1,
                name: "high".into(),
                activate_above_kbps: 1800,
            },
        ]
    }

    #[test]
    fn a_single_layer_never_switches() {
        let mut s = LayerSelector::new(
            vec![VideoLayer {
                id: 0,
                name: "only".into(),
                activate_above_kbps: 0,
            }],
            3000,
        )
        .unwrap();
        for kbps in [3000, 100, 5000, 0] {
            assert_eq!(s.step(kbps), None);
        }
        assert_eq!(s.current(), 0);
    }

    #[test]
    fn no_layers_is_not_a_selector() {
        assert!(LayerSelector::new(vec![], 3000).is_none());
    }

    #[test]
    fn starts_on_the_layer_the_link_already_affords() {
        // A session opening on a good link should not begin low and climb.
        assert_eq!(LayerSelector::new(ladder(), 3000).unwrap().current(), 1);
        assert_eq!(LayerSelector::new(ladder(), 900).unwrap().current(), 0);
    }

    #[test]
    fn drops_immediately_when_the_target_falls_below_the_rung() {
        let mut s = LayerSelector::new(ladder(), 3000).unwrap();
        let d = s.step(1200).expect("must drop at once, not after a dwell");
        assert_eq!((d.layer, d.reason), (0, "down"));
    }

    #[test]
    fn climbs_only_past_the_margin() {
        let mut s = LayerSelector::new(ladder(), 500).unwrap();
        // Exactly at the activation point is not enough: 1800 * 1.25 = 2250.
        for _ in 0..UP_HOLD_S * 2 {
            assert_eq!(s.step(2249), None);
        }
        for _ in 0..UP_HOLD_S - 1 {
            assert_eq!(s.step(2250), None, "must hold the whole window first");
        }
        let u = s
            .step(2250)
            .expect("clears the margin for the whole window");
        assert_eq!((u.layer, u.reason), (1, "up"));
    }

    #[test]
    fn one_second_below_the_margin_restarts_the_window() {
        let mut s = LayerSelector::new(ladder(), 500).unwrap();
        for _ in 0..UP_HOLD_S - 1 {
            s.step(3000);
        }
        assert_eq!(s.step(2000), None, "a dip resets the hold");
        for _ in 0..UP_HOLD_S - 1 {
            assert_eq!(s.step(3000), None);
        }
        assert!(s.step(3000).is_some());
    }

    #[test]
    fn will_not_climb_straight_back_after_dropping() {
        let mut s = LayerSelector::new(ladder(), 3000).unwrap();
        s.step(1000).expect("drops to base");
        // Plenty affordable straight away, but the window starts now.
        assert_eq!(s.step(4000), None);
    }

    #[test]
    fn the_abr_sawtooth_does_not_flap_the_ladder() {
        // Measured on the demo camera at 12 % injected loss: the controller cuts
        // on congestion and recovers 10 % per 5 clean seconds, so the target
        // cycles roughly 1725 -> 2296 -> 1725 about every 15 s. Sampling
        // affordability at one instant climbed on each peak and dropped again
        // seconds later; the pilot paid a decoder reconfigure every time.
        let cycle: Vec<u32> = [1725, 1897, 2087, 2296, 1722, 1894, 2083, 2291]
            .iter()
            .flat_map(|&k| std::iter::repeat(k).take(4))
            .collect();
        let mut s = LayerSelector::new(ladder(), 3000).unwrap();
        let mut switches = 0;
        for _ in 0..6 {
            for &k in &cycle {
                if s.step(k).is_some() {
                    switches += 1;
                }
            }
        }
        // One drop onto the base rung, and then it stays there: no peak of the
        // sawtooth is sustained long enough to buy the high rung back.
        assert_eq!(switches, 1, "layer flapped on the ABR sawtooth");
        assert_eq!(s.current(), 0);
    }

    #[test]
    fn a_link_hovering_at_the_edge_does_not_flap() {
        // The failure this guards against: one switch per second across the
        // activation point, each costing the pilot a decoder reconfigure.
        let mut s = LayerSelector::new(ladder(), 3000).unwrap();
        let mut switches = 0;
        for i in 0..60 {
            let kbps = if i % 2 == 0 { 1750 } else { 1900 };
            if s.step(kbps).is_some() {
                switches += 1;
            }
        }
        assert!(
            switches <= 1,
            "{switches} switches while hovering at the rung edge"
        );
    }

    #[test]
    fn the_switch_driven_feedback_loop_converges() {
        // The loop measured on the demo camera under 12 % loss: while on the low
        // rung the link looks healthy and the target climbs, but relaying the
        // high rung puts enough extra on the wire that the controller cuts and
        // demotes again. A fixed hold cannot damp this, because the cycle is
        // caused by the switch itself. Backoff must make it settle.
        let mut s = LayerSelector::new(ladder(), 3000).unwrap();
        let mut switches = 0;
        let (mut on_high_s, mut t) = (0u32, 0u32);
        for _ in 0..2000 {
            // Target recovers while low, collapses a few seconds after climbing.
            let target = if s.current() == 1 {
                on_high_s += 1;
                if on_high_s > 4 {
                    1725
                } else {
                    2300
                }
            } else {
                on_high_s = 0;
                2300
            };
            if s.step(target).is_some() {
                switches += 1;
            }
            t += 1;
        }
        // Without backoff this cycled forever, roughly once every 15 s — about
        // 130 switches over this run.
        assert!(
            switches <= 10,
            "{switches} switches in {t}s: still flapping"
        );
        assert_eq!(
            s.current(),
            0,
            "should settle on the rung the link sustains"
        );
    }

    #[test]
    fn a_demotion_off_the_starting_rung_teaches_nothing() {
        // The selector opens on whatever the ABR's starting ceiling affords,
        // without having proven the link carries it. Treating that as evidence
        // barred the rung permanently: the learned threshold landed above the
        // profile ceiling, so a fully recovered link stayed on the low rung.
        // Measured on the demo camera — clean link, target back at 2300, still
        // 640x480.
        let mut s = LayerSelector::new(ladder(), 3000).unwrap();
        assert_eq!(s.current(), 1);
        s.step(0).expect("demote off the rung we started on");
        for _ in 0..UP_HOLD_S - 1 {
            assert_eq!(s.step(2300), None);
        }
        assert!(
            s.step(2300).is_some(),
            "a recovered link must climb back; nothing was proven by the opening demotion"
        );
    }

    #[test]
    fn a_link_that_recovers_is_eventually_probed_again() {
        // Learning what failed must not be permanent: a link that genuinely
        // improves has to get its picture back, and trying is the only way to
        // find out.
        let mut s = LayerSelector::new(ladder(), 500).unwrap();
        for _ in 0..UP_HOLD_S {
            s.step(3000);
        }
        assert_eq!(s.current(), 1, "a genuine climb, so the demotion teaches");
        s.step(0).expect("demote, learning that 3000 did not hold");
        // Straight away, even the same healthy target will not buy the rung back.
        for _ in 0..UP_HOLD_S * 3 {
            assert_eq!(s.step(3000), None);
        }
        // After the forgiveness window it probes upward again.
        let mut climbed = false;
        for _ in 0..FORGIVE_S + UP_HOLD_S + 2 {
            if s.step(3000).is_some() {
                climbed = true;
            }
        }
        assert!(climbed, "a recovered link must eventually be probed");
    }

    #[test]
    fn a_sustained_recovery_does_climb() {
        let mut s = LayerSelector::new(ladder(), 500).unwrap();
        let mut ended_high = false;
        for _ in 0..30 {
            if s.step(3000).is_some() {
                ended_high = true;
            }
        }
        assert!(ended_high && s.current() == 1);
    }

    #[test]
    fn three_rungs_step_down_to_what_the_target_affords() {
        let layers = vec![
            VideoLayer {
                id: 0,
                name: "low".into(),
                activate_above_kbps: 0,
            },
            VideoLayer {
                id: 1,
                name: "mid".into(),
                activate_above_kbps: 1200,
            },
            VideoLayer {
                id: 2,
                name: "high".into(),
                activate_above_kbps: 3000,
            },
        ];
        let mut s = LayerSelector::new(layers, 4000).unwrap();
        assert_eq!(s.current(), 2);
        // A collapse to 1300 lands on mid, not all the way at the base.
        assert_eq!(s.step(1300).unwrap().layer, 1);
        assert_eq!(s.step(200).unwrap().layer, 0);
    }
}
