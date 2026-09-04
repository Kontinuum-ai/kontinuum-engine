//! Filters: a TPT (zero-delay-feedback) state-variable filter and a one-pole
//! highpass. The TPT form is unconditionally stable for any cutoff and
//! resonance — no numeric blowups on the render path.

use crate::{InsertFx, ParamId, DENORMAL_FLOOR};
use std::f32::consts::PI;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterMode {
    LowPass,
    HighPass,
    BandPass,
}

/// TPT/ZDF state-variable filter (Zavalishin). Stable for all `fc < sr/2`
/// and any `Q`; cutoff is clamped to 0.45·sr for headroom.
pub struct Svf {
    sr: f32,
    g: f32,
    k: f32,
    ic1: f32,
    ic2: f32,
}

impl Svf {
    pub fn new(sample_rate: u32, cutoff_hz: f32, resonance: f32) -> Self {
        let sr = sample_rate as f32;
        let mut s = Svf { sr, g: 0.0, k: 1.0, ic1: 0.0, ic2: 0.0 };
        s.set_resonance(resonance);
        s.set_cutoff(cutoff_hz);
        s
    }

    pub fn set_cutoff(&mut self, hz: f32) {
        let fc = hz.clamp(20.0, self.sr * 0.45);
        self.g = (PI * fc / self.sr).tan();
    }

    pub fn set_resonance(&mut self, resonance: f32) {
        // k = 1/Q; Q sweeps ~0.55 .. ~10 across the 0..1 resonance range.
        self.k = 1.0 / (0.55 + resonance.clamp(0.0, 1.0) * 9.45);
    }

    pub fn process(&mut self, x: f32, mode: FilterMode) -> f32 {
        let a1 = 1.0 / (1.0 + self.g * (self.g + self.k));
        let a2 = self.g * a1;
        let a3 = self.g * a2;
        let v3 = x - self.ic2;
        let v1 = a1 * self.ic1 + a2 * v3;
        let v2 = self.ic2 + a2 * self.ic1 + a3 * v3;
        self.ic1 = 2.0 * v1 - self.ic1;
        self.ic2 = 2.0 * v2 - self.ic2;
        if self.ic1.abs() < DENORMAL_FLOOR {
            self.ic1 = 0.0;
        }
        if self.ic2.abs() < DENORMAL_FLOOR {
            self.ic2 = 0.0;
        }
        match mode {
            FilterMode::LowPass => v2,
            FilterMode::BandPass => v1,
            FilterMode::HighPass => x - self.k * v1 - v2,
        }
    }

    pub fn process_lowpass(&mut self, x: f32) -> f32 {
        self.process(x, FilterMode::LowPass)
    }

    pub fn reset(&mut self) {
        self.ic1 = 0.0;
        self.ic2 = 0.0;
    }
}

/// Cascaded one-pole highpass — cheap brightness shaping.
#[derive(Clone, Debug)]
pub struct OnePoleHp {
    a: f32,
    lp: f32,
}

impl OnePoleHp {
    pub fn new(sample_rate: f32, cutoff_hz: f32) -> Self {
        OnePoleHp { a: 1.0 - (-std::f32::consts::TAU * cutoff_hz / sample_rate).exp(), lp: 0.0 }
    }

    pub fn process(&mut self, x: f32) -> f32 {
        self.lp += self.a * (x - self.lp);
        x - self.lp
    }

    pub fn reset(&mut self) {
        self.lp = 0.0;
    }
}

/// SVF as a mixer insert — the per-track EQ of static mix staging
/// (issue #27 static v0, #52 WS2). Params: [`crate::params::FILTER_CUTOFF`],
/// [`crate::params::FILTER_RESONANCE`], [`crate::params::FILTER_TYPE`]
/// (0 = lowpass, 1 = highpass, 2 = bandpass).
pub struct FilterInsert {
    svf: Svf,
    mode: FilterMode,
}

impl FilterInsert {
    pub fn new(sample_rate: u32, cutoff_hz: f32, resonance: f32, mode: FilterMode) -> Self {
        FilterInsert { svf: Svf::new(sample_rate, cutoff_hz, resonance), mode }
    }
}

impl InsertFx for FilterInsert {
    fn render(&mut self, io: &mut [f32]) {
        for slot in io.iter_mut() {
            *slot = self.svf.process(*slot, self.mode);
        }
    }

    fn set_param(&mut self, param: ParamId, value: f32) {
        use crate::params::{FILTER_CUTOFF, FILTER_RESONANCE, FILTER_TYPE};
        match param {
            FILTER_CUTOFF => self.svf.set_cutoff(value),
            FILTER_RESONANCE => self.svf.set_resonance(value),
            FILTER_TYPE => {
                self.mode = if value < 0.5 {
                    FilterMode::LowPass
                } else if value < 1.5 {
                    FilterMode::HighPass
                } else {
                    FilterMode::BandPass
                };
            }
            _ => {}
        }
    }

    fn reset(&mut self) {
        self.svf.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::{FILTER_CUTOFF, FILTER_TYPE};

    #[test]
    fn svf_never_blows_up_at_extremes() {
        // Worst case: max cutoff, max resonance, impulsive input — the exact
        // regime the old Chamberlin filter exploded in.
        let mut svf = Svf::new(48_000, 8_000.0, 1.0);
        let mut buf = [0.0f32; 4800];
        for (i, slot) in buf.iter_mut().enumerate() {
            let x = if i % 48 == 0 { 1.0 } else { 0.0 };
            *slot = svf.process(x, FilterMode::LowPass);
        }
        assert!(buf.iter().all(|s| s.is_finite()), "SVF produced non-finite samples");
        let peak = buf.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak < 8.0, "SVF ringing out of control: {peak}");

        let mut svf = Svf::new(48_000, 20_000.0, 1.0);
        let mut last = 0.0f32;
        for i in 0..48_000 {
            last = svf.process(if i % 24 == 0 { 0.8 } else { 0.0 }, FilterMode::HighPass);
        }
        assert!(last.is_finite());
    }

    #[test]
    fn lowpass_shapes_a_step() {
        let mut svf = Svf::new(48_000, 500.0, 0.0);
        let steady: f32 = (0..4800).map(|_| svf.process_lowpass(1.0)).last().unwrap();
        assert!((steady - 1.0).abs() < 0.02, "lowpass must converge to DC gain 1: {steady}");
    }

    #[test]
    fn filter_insert_hums_and_holds_tone() {
        let mut f = FilterInsert::new(48_000, 1000.0, 0.1, FilterMode::LowPass);
        let mut buf = [0.0f32; 4800];
        for (i, slot) in buf.iter_mut().enumerate() {
            *slot = if i % 43 == 0 { 0.9 } else { 0.0 };
        }
        f.render(&mut buf);
        assert!(buf.iter().all(|s| s.is_finite()));

        f.set_param(FILTER_CUTOFF, 4000.0);
        f.set_param(FILTER_TYPE, 1.0);
        f.render(&mut buf);
        assert!(buf.iter().all(|s| s.is_finite()));
    }
}
