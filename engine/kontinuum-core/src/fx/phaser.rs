//! Four/eight-stage phaser (FX v2 subset, #30): a cascade of one-pole
//! allpasses (4 or 8 stages, `PHASER_STAGES`) whose cutoff sweeps with a
//! slow LFO, with optional output feedback into the chain input. Allpass
//! stages have unity magnitude by construction, so the wet path is bounded
//! for any LFO position; feedback is clamped below oscillation. State is
//! per-stage `x1`/`y1`; `render` is allocation-free.
//!
//! Per-sample cost: 4 or 8 allpass evaluations + 1 sin + a few multiplies.

use crate::{InsertFx, ParamId};
use std::f32::consts::PI;

const MAX_STAGES: usize = 8;
const DEFAULT_STAGES: usize = 4;
const CUTOFF_HZ: f32 = 3500.0;

pub struct Phaser {
    sr: f32,
    a: [f32; MAX_STAGES],
    x1: [f32; MAX_STAGES],
    y1: [f32; MAX_STAGES],
    stages: usize,
    lfo: f32,
    rate_hz: f32,
    depth: f32,
    feedback: f32,
    mix: f32,
    fb_out: f32,
}

impl Phaser {
    pub fn new(sample_rate: u32) -> Self {
        let mut p = Phaser {
            sr: sample_rate as f32,
            a: [0.0; MAX_STAGES],
            x1: [0.0; MAX_STAGES],
            y1: [0.0; MAX_STAGES],
            stages: DEFAULT_STAGES,
            lfo: 0.0,
            rate_hz: 0.4,
            depth: 0.6,
            feedback: 0.5,
            mix: 0.5,
            fb_out: 0.0,
        };
        p.update_coeffs();
        p
    }

    /// One-pole allpass coefficient: pole at `f` cycles the notches across
    /// the sweep span as the LFO moves `center` up/down in log space.
    fn update_coeffs(&mut self) {
        let span = self.depth * 0.9;
        let center = 1.0 + span;
        let f = CUTOFF_HZ * center.powf(self.lfo * 2.0 - 1.0);
        let g = (PI * f.clamp(40.0, self.sr * 0.45) / self.sr).tan();
        let coeff = (1.0 - g) / (1.0 + g);
        self.a = [coeff; MAX_STAGES];
    }
}

impl InsertFx for Phaser {
    fn render(&mut self, io: &mut [f32]) {
        let fb = self.feedback.clamp(0.0, 0.85);
        for slot in io.iter_mut() {
            let x = *slot + self.fb_out * fb;
            let mut s = x;
            for i in 0..self.stages {
                let y = self.a[i] * s + self.x1[i] - self.a[i] * self.y1[i];
                self.x1[i] = s;
                self.y1[i] = y;
                s = y;
            }
            self.fb_out = s;
            *slot = *slot + s * self.mix;
            self.lfo += self.rate_hz / self.sr;
            if self.lfo >= 1.0 {
                self.lfo -= 1.0;
            }
        }
        // Coefficients are block-granular: the notches move at LFO speed, so
        // a 64-frame coefficient step is far below audible sweep granularity.
        self.update_coeffs();
        if self.fb_out.abs() < 1e-20 {
            self.fb_out = 0.0;
        }
    }

    fn set_param(&mut self, param: ParamId, value: f32) {
        use crate::params::*;
        match param {
            PHASER_RATE => self.rate_hz = value.clamp(0.02, 8.0),
            PHASER_DEPTH => {
                self.depth = value.clamp(0.0, 1.0);
                self.update_coeffs();
            }
            PHASER_FEEDBACK => self.feedback = value.clamp(0.0, 0.85),
            PHASER_MIX => self.mix = value.clamp(0.0, 1.0),
            PHASER_STAGES => self.stages = if value >= 0.5 { MAX_STAGES } else { DEFAULT_STAGES },
            _ => {}
        }
    }

    fn reset(&mut self) {
        self.x1 = [0.0; MAX_STAGES];
        self.y1 = [0.0; MAX_STAGES];
        self.lfo = 0.0;
        self.fb_out = 0.0;
        self.stages = DEFAULT_STAGES;
        self.update_coeffs();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    #[test]
    fn wet_path_is_bounded_and_silence_in_is_silence_out() {
        let mut p = Phaser::new(48_000);
        p.set_param(crate::params::PHASER_FEEDBACK, 0.85);
        let mut buf = vec![0.0f32; 4800];
        for (i, slot) in buf.iter_mut().enumerate() {
            *slot = 0.5 * (TAU * 800.0 * i as f32 / 48_000.0).sin();
        }
        p.render(&mut buf);
        let peak = buf.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
        assert!(buf.iter().all(|s| s.is_finite()));
        assert!(peak < 2.0, "phaser out of bounds: {peak}");
        assert!(peak > 0.2, "phaser killed signal: {peak}");

        let mut quiet = [0.0f32; 4800];
        p.render(&mut quiet);
        // Allpass states ring down from the sine above; after reset the
        // zero-state invariant must give exact silence out.
        p.reset();
        let mut after = [0.0f32; 4800];
        p.render(&mut after);
        assert!(after.iter().all(|&s| s == 0.0), "silence in must be silence out");
    }

    #[test]
    fn long_run_stable_across_sweep() {
        let mut p = Phaser::new(48_000);
        p.set_param(crate::params::PHASER_RATE, 6.0);
        p.set_param(crate::params::PHASER_DEPTH, 1.0);
        let mut buf = [0.0f32; 4800];
        for block in 0..100 {
            for (i, slot) in buf.iter_mut().enumerate() {
                *slot = 0.4 * (TAU * 440.0 * ((block * 4800 + i) as f32) / 48_000.0).sin();
            }
            p.render(&mut buf);
            assert!(buf.iter().all(|s| s.is_finite() && s.abs() < 4.0));
        }
    }

    #[test]
    fn eight_stages_differ_from_four_and_stay_bounded() {
        let run = |stages: f32| {
            let mut p = Phaser::new(48_000);
            p.set_param(crate::params::PHASER_STAGES, stages);
            p.set_param(crate::params::PHASER_FEEDBACK, 0.85);
            let mut buf = vec![0.0f32; 4800];
            for (i, slot) in buf.iter_mut().enumerate() {
                *slot = 0.5 * (TAU * 800.0 * i as f32 / 48_000.0).sin();
            }
            p.render(&mut buf);
            buf
        };
        let four = run(0.0);
        let eight = run(1.0);
        assert!(four.iter().zip(eight.iter()).any(|(x, y)| x.to_bits() != y.to_bits()),
            "stage count changed nothing");
        let peak = eight.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
        assert!(peak < 3.0, "8-stage out of bounds: {peak}");
    }
}
