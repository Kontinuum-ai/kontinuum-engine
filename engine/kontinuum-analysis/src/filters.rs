//! RBJ biquad filters and BS.1770 K-weighting for the critic (issue
//! #25) — the same shapes and constants as `kontinuum-mastering::filters`
//! / `::loudness`, so the two implementations stay cross-checkable
//! without either crate depending on the other. All state is allocated
//! at construction; `tick` paths allocate nothing.

/// Values under this magnitude flush to zero (matches kontinuum-core).
const DENORMAL_FLOOR: f64 = 1e-20;

// K-weighting (identical to kontinuum-mastering::loudness).
/// K-weighting shelf (spec: +3.99984 dB @ 1681.97 Hz, Q 0.70718).
const SHELF_F0: f64 = 1681.974_450_955_532;
const SHELF_GAIN_DB: f64 = 3.999_843_853_973_347;
const SHELF_Q: f64 = 0.707_175_236_955_420;
/// K-weighting rumble high-pass (spec: 38.135 Hz, Q 0.50033).
const HP_F0: f64 = 38.135_470_876_024_44;
const HP_Q: f64 = 0.500_327_037_323_877;
/// −0.691 + 10·log10(mean square), the BS.1770 calibration offset.
const OFFSET: f64 = -0.691;
/// Mean-square floor: digital silence reads ≈ −240.7 LUFS. Finite on
/// purpose — `f64::NEG_INFINITY` is not representable in JSON, and the
/// snapshots feed #26/#15 over serde.
const MS_FLOOR: f64 = 1e-24;

/// LUFS-style level of a mean-square value, floored (never `-inf`/NaN).
pub fn lufs_db(mean_square: f64) -> f64 {
    OFFSET + 10.0 * mean_square.max(MS_FLOOR).log10()
}

/// RBJ audio-EQ-cookbook biquad (transposed direct form 2, f64 state,
/// f32 sample path) — same shape as `kontinuum-mastering::filters::Biquad`.
#[derive(Clone, Copy, Debug)]
pub struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    z1: f64,
    z2: f64,
}

impl Biquad {
    pub fn identity() -> Self {
        Biquad { b0: 1.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0, z1: 0.0, z2: 0.0 }
    }

    pub fn set_coeffs(&mut self, c: [f64; 5]) {
        self.b0 = c[0];
        self.b1 = c[1];
        self.b2 = c[2];
        self.a1 = c[3];
        self.a2 = c[4];
    }

    pub fn tick(&mut self, x: f32) -> f32 {
        let y = self.b0 * x as f64 + self.z1;
        self.z1 = self.b1 * x as f64 - self.a1 * y + self.z2;
        self.z2 = self.b2 * x as f64 - self.a2 * y;
        if self.z1.abs() < DENORMAL_FLOOR {
            self.z1 = 0.0;
        }
        if self.z2.abs() < DENORMAL_FLOOR {
            self.z2 = 0.0;
        }
        y as f32
    }
}

/// RBJ high-shelf coefficients (positive `gain_db` boosts above `f0`).
pub fn high_shelf_coeffs(sample_rate: f64, f0: f64, gain_db: f64, q: f64) -> [f64; 5] {
    let a = 10.0f64.powf(gain_db / 40.0);
    let w0 = std::f64::consts::TAU * f0 / sample_rate;
    let (s, c) = w0.sin_cos();
    let alpha = s / (2.0 * q);
    let two_sq_a_alpha = 2.0 * a.sqrt() * alpha;
    let b0 = a * ((a + 1.0) + (a - 1.0) * c + two_sq_a_alpha);
    let b1 = -2.0 * a * ((a - 1.0) + (a + 1.0) * c);
    let b2 = a * ((a + 1.0) + (a - 1.0) * c - two_sq_a_alpha);
    let a0 = (a + 1.0) - (a - 1.0) * c + two_sq_a_alpha;
    let a1 = 2.0 * ((a - 1.0) - (a + 1.0) * c);
    let a2 = (a + 1.0) - (a - 1.0) * c - two_sq_a_alpha;
    [b0 / a0, b1 / a0, b2 / a0, a1 / a0, a2 / a0]
}

/// RBJ highpass coefficients.
pub fn highpass_coeffs(sample_rate: f64, f0: f64, q: f64) -> [f64; 5] {
    let w0 = std::f64::consts::TAU * f0 / sample_rate;
    let (s, c) = w0.sin_cos();
    let alpha = s / (2.0 * q);
    let b0 = (1.0 + c) / 2.0;
    let b1 = -(1.0 + c);
    let b2 = (1.0 + c) / 2.0;
    let a0 = 1.0 + alpha;
    let a1 = -2.0 * c;
    let a2 = 1.0 - alpha;
    [b0 / a0, b1 / a0, b2 / a0, a1 / a0, a2 / a0]
}

/// RBJ lowpass coefficients.
pub fn lowpass_coeffs(sample_rate: f64, f0: f64, q: f64) -> [f64; 5] {
    let w0 = std::f64::consts::TAU * f0 / sample_rate;
    let (s, c) = w0.sin_cos();
    let alpha = s / (2.0 * q);
    let b0 = (1.0 - c) / 2.0;
    let b1 = 1.0 - c;
    let b2 = (1.0 - c) / 2.0;
    let a0 = 1.0 + alpha;
    let a1 = -2.0 * c;
    let a2 = 1.0 - alpha;
    [b0 / a0, b1 / a0, b2 / a0, a1 / a0, a2 / a0]
}

/// ITU-R BS.1770 K-weighting, one instance per signal path (shelf →
/// rumble high-pass per channel, the same order as mastering).
pub struct KWeighter {
    shelf: [Biquad; 2],
    hp: [Biquad; 2],
}

impl KWeighter {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f64;
        let shelf = high_shelf_coeffs(sr, SHELF_F0, SHELF_GAIN_DB, SHELF_Q);
        let hp = highpass_coeffs(sr, HP_F0, HP_Q);
        let mut kw = KWeighter { shelf: [Biquad::identity(); 2], hp: [Biquad::identity(); 2] };
        for ch in 0..2 {
            kw.shelf[ch].set_coeffs(shelf);
            kw.hp[ch].set_coeffs(hp);
        }
        kw
    }

    /// One stereo frame; returns the K-weighted (L, R).
    pub fn tick(&mut self, l: f32, r: f32) -> (f32, f32) {
        (self.hp[0].tick(self.shelf[0].tick(l)), self.hp[1].tick(self.shelf[1].tick(r)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kweighted_sine_lufs_lands_near_the_spec_value() {
        // Dual-mono full-scale 997 Hz sine: spec value ≈ 0 LUFS
        // (−0.691 + K-gain ≈ +0.7 dB at 1 kHz), EBU Tech 3341 convention.
        let sr = 48_000u32;
        let mut kw = KWeighter::new(sr);
        let mut ms = 0.0f64;
        let n = sr as usize * 4;
        for i in 0..n {
            let x = (std::f64::consts::TAU * 997.0 * i as f64 / sr as f64).sin() as f32;
            let (kl, kr) = kw.tick(x, x);
            ms += (kl * kl + kr * kr) as f64;
        }
        let lufs = lufs_db(ms / n as f64);
        assert!(lufs.abs() < 1.0, "997 Hz full-scale sine must read ≈ 0 LUFS, got {lufs}");
    }
}

