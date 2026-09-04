//! Stage 2 — dynamic low control (#28): sub-rumble discipline in the
//! 30–50 Hz node.
//!
//! Design approximation, stated honestly: this is a *dynamically driven
//! low shelf* (corner 50 Hz, up to −6 dB), not a surgical 30–50 Hz
//! band node. A shelf avoids the phase/summing artifacts of a split-band
//! dynamic EQ at real-time cost, and sub rumble discipline is a broad
//! "duck what lives under the kick" move. Detection lowpasses at 42 Hz
//! (two cascaded RBJ lowpasses ≈ 24 dB/oct), so only sub-heavy content
//! engages it.
//!
//! Bounded: the reduction is hard-capped at [`LOW_CONTROL_MAX_DB`] and
//! slews at audio-safe speed, so a DC-wrecked or rumble-flooded input
//! degrades to "gently duller lows", never pumping holes.

use crate::filters::{low_shelf_coeffs, lowpass_coeffs, Biquad, Slew1p};

/// Hard cap on the dynamic cut.
pub const LOW_CONTROL_MAX_DB: f32 = 6.0;

/// Detection corner — top of the 30–50 Hz node.
const DETECT_HZ: f64 = 42.0;
/// Detection slope: two cascaded Q=0.707 lowpasses.
const DETECT_STAGES: usize = 2;
/// Applied shelf corner (50 Hz keeps the cut out of kick body).
const SHELF_HZ: f64 = 50.0;
/// Sub level above this (dBFS) starts engaging the cut.
const THRESHOLD_DB: f32 = -18.0;
/// dB of detection level over threshold that reaches the full cut.
const RANGE_DB: f32 = 12.0;
/// Ballistics: quick enough to grab a rumble, slow enough not to saw.
const ATTACK_MS: f32 = 15.0;
const RELEASE_MS: f32 = 250.0;
/// Reduction smoothing (audio-rate zipper guard).
const GR_TAU_MS: f32 = 60.0;

fn one_pole_coeff(sample_rate: f32, tau_ms: f32) -> f32 {
    (-1000.0 / (tau_ms.max(0.01) * sample_rate)).exp()
}

/// Stereo-linked dynamic low control.
pub struct DynamicLowControl {
    sample_rate: f64,
    detect: [[Biquad; DETECT_STAGES]; 2],
    /// Linear envelope of the linked sub-band level.
    env: f32,
    attack_coeff: f32,
    release_coeff: f32,
    /// Smoothed gain reduction (negative dB), applied via the shelf.
    gr_db: Slew1p,
    /// Breakdown relaxation 0..1 (0 = full intensity).
    relax: f32,
    low_shelf: [Biquad; 2],
}

impl DynamicLowControl {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f64;
        let sr32 = sample_rate as f32;
        let det = [lowpass_coeffs(sr, DETECT_HZ, 0.707); DETECT_STAGES];
        let mut stages = [[Biquad::identity(), Biquad::identity()]; 2];
        for ch in 0..2 {
            for st in 0..DETECT_STAGES {
                stages[ch][st].set_coeffs(det[st]);
            }
        }
        let mut ctl = DynamicLowControl {
            sample_rate: sr,
            detect: stages,
            env: 0.0,
            attack_coeff: 1.0 - one_pole_coeff(sr32, ATTACK_MS),
            release_coeff: one_pole_coeff(sr32, RELEASE_MS),
            gr_db: Slew1p::new(sr32, GR_TAU_MS),
            relax: 0.0,
            low_shelf: [Biquad::identity(), Biquad::identity()],
        };
        ctl.gr_db.snap(0.0);
        ctl.update_shelf();
        ctl
    }

    /// Section-aware relaxation (0 = full discipline, 1 = breakdown).
    pub fn set_relax(&mut self, relax: f32) {
        self.relax = relax.clamp(0.0, 1.0);
    }

    /// Advance the per-sample detector + GR smoothing. The shelf
    /// coefficients follow once per block (the 60 ms GR slew keeps the
    /// step inaudible).
    pub fn tick(&mut self, left: f32, right: f32) -> (f32, f32) {
        let dl = self.detect_sub(0, left);
        let dr = self.detect_sub(1, right);
        let linked = dl.max(dr).max(0.0);
        // Peak envelope: instant-ish grab, slow release.
        if linked > self.env {
            self.env += self.attack_coeff * (linked - self.env);
        } else {
            self.env *= self.release_coeff;
        }
        let env_db = 20.0 * (self.env as f64).max(1e-12).log10() as f32;
        let thr = THRESHOLD_DB + self.relax * 9.0;
        let x = ((env_db - thr) / RANGE_DB).clamp(0.0, 1.0);
        // Smooth quadratic knee into the cut.
        let wanted = -LOW_CONTROL_MAX_DB * x * (2.0 - x);
        self.gr_db.set_target(wanted);
        let gr = self.gr_db.tick();
        (gr, gr)
    }

    fn detect_sub(&mut self, ch: usize, x: f32) -> f32 {
        let mut y = x;
        for st in 0..DETECT_STAGES {
            y = self.detect[ch][st].tick(y);
        }
        y.abs()
    }

    /// Recompute the applied shelf from the current reduction. Called
    /// once per render block by the chain.
    pub fn update_block(&mut self) {
        self.update_shelf();
    }

    fn update_shelf(&mut self) {
        let c = low_shelf_coeffs(self.sample_rate, SHELF_HZ, self.gr_db.value() as f64, 0.707);
        for ch in 0..2 {
            self.low_shelf[ch].set_coeffs(c);
        }
    }

    /// Apply the shelf to a stereo frame. (Split from `tick` so the
    /// chain controls coefficient refresh granularity.)
    pub fn apply(&mut self, left: f32, right: f32) -> (f32, f32) {
        (self.low_shelf[0].tick(left), self.low_shelf[1].tick(right))
    }

    /// Current reduction in dB (0 = untouched, positive = cutting).
    pub fn gr_db(&self) -> f32 {
        -self.gr_db.value()
    }

    pub fn reset(&mut self) {
        for ch in 0..2 {
            for st in 0..DETECT_STAGES {
                self.detect[ch][st].reset();
            }
            self.low_shelf[ch].reset();
        }
        self.env = 0.0;
        self.gr_db.snap(0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sub_rumble_is_cut_bounded_and_mids_pass() {
        let sr = 48_000u32;
        // Hot 40 Hz sine engages the discipline.
        let mut ctl = DynamicLowControl::new(sr);
        let n = sr as usize;
        let mut out40 = 0.0f64;
        for i in 0..n {
            let x = 0.5 * (std::f32::consts::TAU * 40.0 * i as f32 / sr as f32).sin();
            let (gr, _) = ctl.tick(x, x);
            let (l, _) = ctl.apply(x, x);
            if i > n / 2 {
                out40 += l as f64 * l as f64;
                assert!(gr <= LOW_CONTROL_MAX_DB + 1e-4, "cap breached: {gr}");
            }
            ctl.update_block();
        }
        out40 = (out40 / (n / 2) as f64).sqrt();
        let in40 = 0.5f64 / std::f64::consts::SQRT_2;
        let cut_db = -20.0 * (out40 / in40).log10();
        assert!(cut_db > 1.5, "40 Hz sine must be reduced: {cut_db} dB");
        assert!(cut_db <= LOW_CONTROL_MAX_DB as f64 + 1.0, "cut {cut_db} dB");

        // 1 kHz sits far above the shelf — essentially untouched.
        let mut ctl = DynamicLowControl::new(sr);
        let mut out1k = 0.0f64;
        for i in 0..n {
            let x = 0.5 * (std::f32::consts::TAU * 1_000.0 * i as f32 / sr as f32).sin();
            ctl.tick(x, x);
            let (l, _) = ctl.apply(x, x);
            if i > n / 2 {
                out1k += l as f64 * l as f64;
            }
            ctl.update_block();
        }
        out1k = (out1k / (n / 2) as f64).sqrt();
        let delta_db = (20.0 * (out1k / in40).log10()).abs();
        assert!(delta_db < 0.5, "1 kHz shift: {delta_db} dB");
    }
}
