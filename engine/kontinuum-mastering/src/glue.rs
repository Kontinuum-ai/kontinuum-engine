//! Stage 3 — glue compression (#28): 1.5–2:1 stereo-linked program
//! compressor with slow attack, program-dependent release, and a slow
//! auto-threshold seeker that holds the mean gain reduction at its 1–2 dB
//! target.
//!
//! Safety story:
//! - ratio bounded to [1.5, 2.0]; threshold bounded [−40, −6] dBFS and
//!   slews ≤ 1 dB/s; makeup bounded [0, 4] dB and slews.
//! - the seeker only *lowers* the threshold while program level is
//!   healthy (> [`SEEKER_FLOOR_DB`]); on quiet or breakdown material it
//!   relaxes the threshold upward toward [`DEFAULT_THRESHOLD_DB`] so a
//!   quiet mix is never pumped to satisfy the GR target.
//! - program-dependent release: the release time constant stretches with
//!   a slow average of the detected level, so sustained sections breathe
//!   slowly while dropouts recover fast.

use crate::filters::Slew1p;

/// Ratio bounds (#28: 1.5–2:1). The chain ships at [`DEFAULT_RATIO`].
pub const RATIO_MIN: f32 = 1.5;
pub const RATIO_MAX: f32 = 2.0;
pub const DEFAULT_RATIO: f32 = 1.8;

/// GR target the seeker holds (dB of reduction).
pub const GR_TARGET_DB: f32 = 1.5;
/// Seeker engages only above this detected level (dBFS).
const SEEKER_FLOOR_DB: f32 = -35.0;
/// Threshold resting point and bounds.
const DEFAULT_THRESHOLD_DB: f32 = -16.0;
const THRESHOLD_MIN_DB: f32 = -40.0;
const THRESHOLD_MAX_DB: f32 = -6.0;
const THRESHOLD_SLEW_DB_PER_S: f32 = 1.0;
/// Seeker gain: threshold dB/s per dB of GR error. Deliberately gentle —
/// the gr_mean lag makes a hot seeker wind up and overshoot.
const SEEKER_GAIN: f32 = 0.5;
/// No threshold movement while mean GR is this close to the target.
const SEEKER_DEADBAND_DB: f32 = 0.15;
/// Averaging window for the mean GR the seeker reacts to.
const GR_MEAN_TAU_MS: f32 = 1_000.0;
/// Makeup bounds.
const MAKEUP_MAX_DB: f32 = 4.0;
/// Detector ballistics: slow attack = glue, release stretched by program.
const ATTACK_MS: f32 = 30.0;
const RELEASE_FAST_MS: f32 = 120.0;
const RELEASE_SLOW_MS: f32 = 420.0;

fn one_pole_coeff(sample_rate: f32, tau_ms: f32) -> f32 {
    (-1000.0 / (tau_ms.max(0.01) * sample_rate)).exp()
}

/// Stereo-linked glue compressor. `tick` advances detection and returns
/// the (gr_db, makeup_db) pair to apply; `apply` is the chain's choice of
/// granularity. GR telemetry is positive dB of reduction.
pub struct GlueCompressor {
    sample_rate: f32,
    ratio: f32,
    threshold_db: f32,
    /// Peak detector of the linked input.
    env: f32,
    attack_coeff: f32,
    /// Slow program level (linear) that stretches the release.
    program: f32,
    /// Slow average of GR (negative dB) — the seeker's measurement.
    gr_mean: f32,
    makeup_db: Slew1p,
    /// Breakdown relaxation 0..1.
    relax: f32,
    /// Per-sample applied gain, smoothed to keep moves click-free.
    gain: f32,
}

impl GlueCompressor {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        let mut glue = GlueCompressor {
            sample_rate: sr,
            ratio: DEFAULT_RATIO,
            threshold_db: DEFAULT_THRESHOLD_DB,
            env: 0.0,
            attack_coeff: 1.0 - one_pole_coeff(sr, ATTACK_MS),
            program: 0.0,
            // Assume on-target at startup so the seeker waits for a real
            // measurement instead of ramping on a zero-initialized mean.
            gr_mean: GR_TARGET_DB,
            makeup_db: Slew1p::new(sr, 1_000.0),
            relax: 0.0,
            gain: 1.0,
        };
        glue.makeup_db.snap(0.0);
        glue
    }

    /// Ratio, clamped to the #28 bounds.
    pub fn set_ratio(&mut self, ratio: f32) {
        self.ratio = if ratio.is_finite() { ratio } else { DEFAULT_RATIO };
        self.ratio = self.ratio.clamp(RATIO_MIN, RATIO_MAX);
    }

    /// Section-aware relaxation (0 = full intensity, 1 = breakdown).
    pub fn set_relax(&mut self, relax: f32) {
        self.relax = relax.clamp(0.0, 1.0);
    }

    /// Advance detection for one stereo frame; returns the gain reduction
    /// (positive dB) and current makeup (positive dB) to apply.
    pub fn tick(&mut self, left: f32, right: f32) -> (f32, f32) {
        let linked = left.abs().max(right.abs()).max(0.0);
        // Program-dependent release: the release coefficient blends from
        // fast (dropouts) to slow (sustained loud program). The slow
        // tracker (τ ≈ 2 s) is normalized so ≈ −12 dBFS counts as "loud".
        let slow_norm = (self.program * 4.0).clamp(0.0, 1.0);
        let rel_tau = RELEASE_FAST_MS + (RELEASE_SLOW_MS - RELEASE_FAST_MS) * slow_norm;
        let release_step = 1.0 - one_pole_coeff(self.sample_rate, rel_tau);
        if linked > self.env {
            self.env += self.attack_coeff * (linked - self.env);
        } else {
            self.env += release_step * (linked - self.env);
        }
        let program_coeff = one_pole_coeff(self.sample_rate, 2_000.0);
        self.program += program_coeff * (self.env - self.program);

        let env_db = 20.0 * (self.env as f64).max(1e-12).log10() as f32;
        // Breakdowns: lift the threshold and let the signal through.
        let thr = self.threshold_db + self.relax * 8.0;
        let over_db = (env_db - thr).max(0.0);
        let gr_db = (1.0 - 1.0 / self.ratio) * over_db;
        let makeup = self.makeup_db.value();

        // Seeker measurement; makeup follows the recovered GR.
        let mean_coeff = one_pole_coeff(self.sample_rate, GR_MEAN_TAU_MS);
        self.gr_mean += mean_coeff * (gr_db - self.gr_mean);
        let makeup_want = if env_db > SEEKER_FLOOR_DB {
            (self.gr_mean * 0.9).clamp(0.0, MAKEUP_MAX_DB)
        } else {
            0.0
        };
        self.makeup_db.set_target(makeup_want * (1.0 - self.relax));
        self.makeup_db.tick();

        // Click guard: slew the applied gain at audio-safe speed (the GR
        // moves are already ballistic; this also covers threshold/makeup
        // steps).
        let target_gain = 10.0f32.powf(-(gr_db + makeup) / 20.0);
        let gain_coeff = one_pole_coeff(self.sample_rate, 5.0);
        self.gain += gain_coeff * (target_gain - self.gain);
        (gr_db, makeup)
    }

    /// Apply the current gain to a stereo frame.
    pub fn apply(&mut self, left: f32, right: f32) -> (f32, f32) {
        (left * self.gain, right * self.gain)
    }

    /// Per-block housekeeping: the threshold seeker (slew-capped, gated
    /// on healthy program level) and relaxation of the makeup target.
    pub fn update_block(&mut self, frames: usize) {
        let dt = frames as f32 / self.sample_rate;
        let env_db = 20.0 * (self.env as f64).max(1e-12).log10() as f32;
        let err = self.gr_mean - GR_TARGET_DB * (1.0 - self.relax);
        let mut step = 0.0;
        if err.abs() > SEEKER_DEADBAND_DB {
            // Too much GR (err > 0) lifts the threshold; too little drops
            // it — the plant gain is positive in thr.
            step = SEEKER_GAIN * err * dt;
            if step.abs() > THRESHOLD_SLEW_DB_PER_S * dt {
                step = step.signum() * THRESHOLD_SLEW_DB_PER_S * dt;
            }
        }
        let mut thr = self.threshold_db + step;
        // Only pull the threshold down while the program is healthy;
        // always relax it back up toward the resting point.
        let floor_ok = env_db > SEEKER_FLOOR_DB;
        if !floor_ok {
            thr = thr.max(DEFAULT_THRESHOLD_DB) + 0.5 * dt;
        }
        thr = thr.clamp(THRESHOLD_MIN_DB, THRESHOLD_MAX_DB);
        // Seek floor: never chase below (resting − 8 dB).
        self.threshold_db = thr.max(DEFAULT_THRESHOLD_DB - 8.0);
    }

    pub fn gr_db(&self) -> f32 {
        self.gr_mean
    }

    /// Detected program level (dBFS) — feeds #25 telemetry.
    pub fn env_db(&self) -> f32 {
        20.0 * (self.env as f64).max(1e-12).log10() as f32
    }

    pub fn threshold_db(&self) -> f32 {
        self.threshold_db
    }

    pub fn makeup_db(&self) -> f32 {
        self.makeup_db.value()
    }

    pub fn reset(&mut self) {
        self.env = 0.0;
        self.program = 0.0;
        // Both restore the constructor's seeds exactly: a zeroed gr_mean
        // would make the seeker ramp from nothing, and the sought threshold
        // would leak program history across the reset (the graph-level
        // determinism contract pins reset == fresh, bit for bit).
        self.gr_mean = GR_TARGET_DB;
        self.threshold_db = DEFAULT_THRESHOLD_DB;
        self.gain = 1.0;
        self.makeup_db.snap(0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Steady loud program: seeker must land GR near the target and stay
    /// inside the 1–2 dB brief (± 0.5 dB of measurement slack). The
    /// slew-capped loop settles over several seconds — asserted in the
    /// tail, with a coarse guard while it converges.
    #[test]
    fn seeker_holds_gr_near_target_on_loud_program() {
        let sr = 48_000u32;
        let mut g = GlueCompressor::new(sr);
        let mut out_rms = 0.0f64;
        let mut count = 0.0f64;
        let tail_start = 8 * sr as usize;
        for i in 0..12 * sr as usize {
            // 100 Hz + 1 kHz mix: healthy program around −8 dBFS peak.
            let x = 0.35 * (std::f32::consts::TAU * 100.0 * i as f32 / sr as f32).sin()
                + 0.1 * (std::f32::consts::TAU * 1_000.0 * i as f32 / sr as f32).sin();
            let (gr, _makeup) = g.tick(x, x);
            let (l, _) = g.apply(x, x);
            g.update_block(1);
            if i > tail_start {
                out_rms += l as f64 * l as f64;
                count += 1.0;
                if i % 48_000 == 0 {
                    assert!(
                        (0.5..=3.0).contains(&gr),
                        "GR wandered: {gr} dB at frame {i}"
                    );
                }
            }
        }
        let rms = (out_rms / count).sqrt();
        assert!(rms > 1e-3, "output collapsed");
        assert!(
            (g.gr_db() - GR_TARGET_DB).abs() < 0.7,
            "mean GR {} off target {GR_TARGET_DB}",
            g.gr_db()
        );
    }

    #[test]
    fn quiet_mix_is_not_pumped() {
        let sr = 48_000u32;
        let mut g = GlueCompressor::new(sr);
        // Very quiet program: the seeker must not chase it downward.
        for i in 0..8 * sr as usize {
            let x = 0.003 * (std::f32::consts::TAU * 440.0 * i as f32 / sr as f32).sin();
            let (gr, _) = g.tick(x, x);
            let (l, _) = g.apply(x, x);
            g.update_block(1);
            assert!(gr < 1.0, "quiet mix pumped by {gr} dB");
            assert!(l.abs() < 0.05);
        }
        assert!(g.threshold_db() >= DEFAULT_THRESHOLD_DB - 0.5, "threshold chased down");
    }
}
