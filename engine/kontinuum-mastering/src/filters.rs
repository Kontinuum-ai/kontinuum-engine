//! Shared DSP primitives for the mastering chain: RBJ biquads with f64
//! state (double-precision states avoid the sub-100 Hz accuracy and
//! denormal trouble of f32 biquads; the sample path stays f32) and the
//! one-pole slew smoother every adaptive parameter moves through.
//!
//! Mirrors `kontinuum-core`'s denormal discipline (`DENORMAL_FLOOR`
//! flush) without depending on that crate — the mastering chain keeps
//! its own primitives so the render path stays allocation-free.

/// Values under this magnitude flush to zero (matches kontinuum-core).
const DENORMAL_FLOOR: f64 = 1e-20;

/// RBJ audio-EQ-cookbook biquad. Coefficients are recomputed by the
/// stage when its (slowly moving) gain parameter changes — at most once
/// per render block, which the per-block slew caps keep click-free.
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
    /// Identity filter (passthrough) — a defined starting state.
    pub fn identity() -> Self {
        Biquad { b0: 1.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0, z1: 0.0, z2: 0.0 }
    }

    /// Transposed-direct-form-2 tick: f32 in, f64 state, f32 out.
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

    pub fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }
}

/// RBJ low-shelf coefficients (cookbook with the Q·√A slope correction
/// folded in). Positive `gain_db` boosts below `f0`.
pub fn low_shelf_coeffs(sample_rate: f64, f0: f64, gain_db: f64, q: f64) -> [f64; 5] {
    let a = 10.0f64.powf(gain_db / 40.0);
    let w0 = std::f64::consts::TAU * f0 / sample_rate;
    let (s, c) = w0.sin_cos();
    let alpha = s / (2.0 * q);
    let two_sq_a_alpha = 2.0 * a.sqrt() * alpha;
    let b0 = a * ((a + 1.0) - (a - 1.0) * c + two_sq_a_alpha);
    let b1 = 2.0 * a * ((a - 1.0) - (a + 1.0) * c);
    let b2 = a * ((a + 1.0) - (a - 1.0) * c - two_sq_a_alpha);
    let a0 = (a + 1.0) + (a - 1.0) * c + two_sq_a_alpha;
    let a1 = -2.0 * ((a - 1.0) + (a + 1.0) * c);
    let a2 = (a + 1.0) + (a - 1.0) * c - two_sq_a_alpha;
    [b0 / a0, b1 / a0, b2 / a0, a1 / a0, a2 / a0]
}

/// RBJ high-shelf coefficients. Positive `gain_db` boosts above `f0`.
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

/// RBJ lowpass coefficients (for detection paths — no resonance).
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

/// RBJ highpass coefficients (K-weighting rumble stage, detection paths).
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

impl Biquad {
    /// Replace coefficients (state untouched — small slew-capped moves
    /// keep the transition inaudible; a full `reset` is the caller's call).
    pub fn set_coeffs(&mut self, c: [f64; 5]) {
        self.b0 = c[0];
        self.b1 = c[1];
        self.b2 = c[2];
        self.a1 = c[3];
        self.a2 = c[4];
    }
}

/// One-pole slew limiter toward a target: the only path an adaptive
/// parameter may take. Hard bounds are applied by the caller via
/// `set_target`; this type owns the "never jumps" guarantee.
#[derive(Clone, Debug)]
pub struct Slew1p {
    current: f32,
    target: f32,
    coeff: f32,
}

impl Slew1p {
    /// One-pole with the -60 dB settling approximation used by
    /// kontinuum-core's `Smoother`: `coeff = exp(-1000 / (tau_ms · sr))`.
    pub fn new(sample_rate: f32, tau_ms: f32) -> Self {
        let coeff = (-1000.0 / (tau_ms.max(0.01) * sample_rate)).exp();
        Slew1p { current: 0.0, target: 0.0, coeff }
    }

    pub fn set_target(&mut self, value: f32) {
        self.target = value;
    }

    pub fn target(&self) -> f32 {
        self.target
    }

    pub fn value(&self) -> f32 {
        self.current
    }

    /// Advance one frame. Always monotone toward the target (no overshoot
    /// by construction), so a step in the target is a smooth exponential.
    pub fn tick(&mut self) -> f32 {
        self.current += (1.0 - self.coeff) * (self.target - self.current);
        self.current
    }

    /// Advance once per render block (for mastering-scale parameters that
    /// must not whipsaw at audio rate).
    pub fn tick_block(&mut self, frames: usize) {
        for _ in 0..frames {
            self.tick();
        }
    }

    pub fn snap(&mut self, value: f32) {
        self.current = value;
        self.target = value;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shelf_hits_its_dc_and_plateau_gain() {
        let sr = 48_000.0;
        // Low shelf +6 dB @ 200 Hz: DC gain must land on +6 dB.
        let mut bq = Biquad::identity();
        bq.set_coeffs(low_shelf_coeffs(sr, 200.0, 6.0, 0.707));
        let mut last = 0.0f32;
        for _ in 0..sample_count(48_000.0, 40.0) {
            last = bq.tick(1.0);
        }
        let dc_db = 20.0 * (last as f64).abs().max(1e-12).log10();
        assert!((dc_db - 6.0).abs() < 0.05, "low shelf DC gain {dc_db} dB");
    }

    #[test]
    fn slew_never_overshoots_and_settles() {
        let mut s = Slew1p::new(48_000.0, 50.0);
        s.set_target(3.0);
        let mut prev = 0.0f32;
        for i in 0..100_000 {
            let v = s.tick();
            assert!(v >= prev - 1e-6, "undershot at {i}");
            assert!(v <= 3.0 + 1e-6, "overshot at {i}");
            prev = v;
        }
        assert!((s.value() - 3.0).abs() < 1e-3, "did not settle: {}", s.value());
    }

    fn sample_count(sr: f64, seconds: f64) -> usize {
        (sr * seconds) as usize
    }
}
