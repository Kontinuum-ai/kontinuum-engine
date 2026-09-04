//! Stage 1 — master tilt EQ (#28): a mirrored low-shelf/high-shelf pair
//! pivoting at the target tilt frequency. Positive tilt target *brightens*
//! (high shelf up, low shelf down); negative darkens.
//!
//! Mastering-scale behavior: the applied gain slews through a 5 s one-pole
//! (a mastering move, not a mix move) and the target is hard-capped at
//! ±3 dB, so even a pathological `set_tilt_target_db` cannot whipsaw or
//! carve the mix. Coefficients are recomputed once per render block from
//! the slewed gain — with the block slew capped by the 5 s time constant
//! the per-block step stays far below audibility.

use crate::filters::{high_shelf_coeffs, low_shelf_coeffs, Biquad, Slew1p};

/// Hard cap on the tilt magnitude. The mix can end up slightly duller or
/// brighter than intended; never destroyed.
pub const TILT_MAX_DB: f32 = 3.0;

/// Time constant of the applied tilt gain — mastering scale.
const TILT_TAU_MS: f32 = 5_000.0;

/// Shelf Q (Butterworth-ish, gentle).
const SHELF_Q: f64 = 0.707;

/// Stereo-linked tilt EQ. Coefficients update per render block.
pub struct TiltEq {
    sample_rate: f64,
    pivot_hz: f64,
    gain_db: Slew1p,
    low: [Biquad; 2],
    high: [Biquad; 2],
}

impl TiltEq {
    pub fn new(sample_rate: u32, pivot_hz: f64) -> Self {
        let mut eq = TiltEq {
            sample_rate: sample_rate as f64,
            pivot_hz: pivot_hz.max(20.0).min(sample_rate as f64 * 0.45),
            gain_db: Slew1p::new(sample_rate as f32, TILT_TAU_MS),
            low: [Biquad::identity(), Biquad::identity()],
            high: [Biquad::identity(), Biquad::identity()],
        };
        eq.update_coeffs();
        eq
    }

    /// Set the corrective tilt. Positive brightens. Clamped to ±3 dB.
    pub fn set_tilt_target_db(&mut self, db: f32) {
        let clamped = if db.is_finite() { db } else { 0.0 };
        self.gain_db.set_target(clamped.clamp(-TILT_MAX_DB, TILT_MAX_DB));
    }

    pub fn tilt_target_db(&self) -> f32 {
        self.gain_db.target()
    }

    /// Recompute shelf coefficients from the current (slewed) gain. Called
    /// once per render block by the chain.
    pub fn update_block(&mut self, frames: usize) {
        self.gain_db.tick_block(frames);
        self.update_coeffs();
    }

    fn update_coeffs(&mut self) {
        let g = self.gain_db.value() as f64;
        let sr = self.sample_rate;
        let f0 = self.pivot_hz;
        // Mirrored pair: low shelf −g, high shelf +g. At the pivot the two
        // shelves' gains cancel, so the pivot stays ~unity (the classic
        // tilt response).
        let low = low_shelf_coeffs(sr, f0, -g, SHELF_Q);
        let high = high_shelf_coeffs(sr, f0, g, SHELF_Q);
        for ch in 0..2 {
            self.low[ch].set_coeffs(low);
            self.high[ch].set_coeffs(high);
        }
    }

    /// Process one stereo frame (in-place channel values).
    pub fn tick(&mut self, left: f32, right: f32) -> (f32, f32) {
        let l = self.high[0].tick(self.low[0].tick(left));
        let r = self.high[1].tick(self.low[1].tick(right));
        (l, r)
    }

    pub fn reset(&mut self) {
        for ch in 0..2 {
            self.low[ch].reset();
            self.high[ch].reset();
        }
        self.gain_db.snap(self.gain_db.target());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settled_amplitude(eq: &mut TiltEq, freq_hz: f32, sr: u32, seconds: f64) -> f64 {
        let n = (sr as f64 * seconds) as usize;
        let mut peak_acc = 0.0f64;
        let mut blocks = 0usize;
        for i in 0..n {
            let x = (std::f32::consts::TAU * freq_hz * i as f32 / sr as f32).sin();
            let (l, _) = eq.tick(x, x);
            peak_acc += l as f64 * l as f64;
            blocks += 1;
        }
        (peak_acc / blocks as f64).sqrt() * std::f64::consts::SQRT_2
    }

    #[test]
    fn tilt_is_hard_capped_at_three_db() {
        let mut eq = TiltEq::new(48_000, 700.0);
        eq.set_tilt_target_db(50.0);
        assert_eq!(eq.tilt_target_db(), 3.0);
        eq.set_tilt_target_db(f32::NAN);
        assert_eq!(eq.tilt_target_db(), 0.0, "non-finite falls back to neutral");
    }

    #[test]
    fn positive_tilt_brightens_the_top_end() {
        let sr = 48_000u32;
        // Neutral reference.
        let mut eq = TiltEq::new(sr, 700.0);
        let flat_hi = settled_amplitude(&mut eq, 8_000.0, sr, 1.0);
        // Brighten fully: +3 dB high shelf at 8 kHz (~1.5 octaves above
        // pivot). The 5 s mastering slew needs ~25 s to settle, advanced
        // in block-sized steps exactly as the chain drives it.
        eq.set_tilt_target_db(TILT_MAX_DB);
        let blocks = (25.0 * sr as f64 / 64.0) as usize;
        for _ in 0..blocks {
            eq.update_block(64);
        }
        let bright_hi = settled_amplitude(&mut eq, 8_000.0, sr, 1.0);
        let delta_db = 20.0 * (bright_hi / flat_hi).log10();
        assert!(
            (1.8..=3.2).contains(&delta_db),
            "8 kHz shift with +3 dB tilt: {delta_db} dB"
        );
    }
}
