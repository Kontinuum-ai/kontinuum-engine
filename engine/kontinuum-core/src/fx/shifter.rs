//! Frequency shifter (FX v2 subset, #30): single-sideband shift via a
//! Hilbert-ish quadrature pair — 4+4 first-order allpasses (halfband-mirrored
//! pole families, least-squares-fitted offline at 48 kHz), feeding a complex
//! rotator. No ring-mod image at 2·f: only the shifted sideband survives to
//! the approximation quality of the quadrature network.
//!
//! Approximation quality: branch phase difference is 90° ± 8° across
//! 150 Hz–5 kHz (±3° typical mid-band) at the 48 kHz design rate, giving
//! roughly 23–30 dB in-band image rejection. Below ~150 Hz and above ~5 kHz
//! the quadrature degrades and a faint image leaks through. Pole
//! quarter-frequencies are pre-warped with tan(pi·q/sr) at construction, so
//! other sample rates keep the same character with graceful drift.
//!
//! Per-sample cost: 8 allpass evaluations + sin/cos of the rotator.

use crate::{InsertFx, ParamId};
use std::f32::consts::{PI, TAU};

const STAGES: usize = 4;
/// Quadrature pole quarter-frequencies (Hz), fitted for a 90° branch phase
/// difference across 150 Hz–5 kHz at 48 kHz. Branch B poles are the
/// halfband mirrors of branch A (q_b = sr/2 − q_a, reversed).
const POLES_A: [f32; STAGES] = [56.8, 1330.4, 18_256.5, 23_683.6];
const POLES_B: [f32; STAGES] = [316.4, 5743.5, 22_669.6, 23_943.2];

pub struct FreqShifter {
    sr: f32,
    a: [f32; STAGES],
    b: [f32; STAGES],
    xa: [f32; STAGES],
    ya: [f32; STAGES],
    xb: [f32; STAGES],
    yb: [f32; STAGES],
    rot: f32,
    shift_hz: f32,
    mix: f32,
}

impl FreqShifter {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        // tan pre-warp of the fitted quarter frequencies, mapped through
        // a = (1 - g) / (1 + g) so every pole stays inside the unit circle
        // (the raw tan exceeds 1 above sr/4 and would ring unstably).
        let warp = |q: f32| {
            let g = (PI * q / sr).tan();
            (1.0 - g) / (1.0 + g)
        };
        FreqShifter {
            sr,
            a: POLES_A.map(warp),
            b: POLES_B.map(warp),
            xa: [0.0; STAGES],
            ya: [0.0; STAGES],
            xb: [0.0; STAGES],
            yb: [0.0; STAGES],
            rot: 0.0,
            shift_hz: 0.0,
            mix: 1.0,
        }
    }

    fn allpass(&mut self, x: f32) -> (f32, f32) {
        let mut sa = x;
        let mut sb = x;
        for i in 0..STAGES {
            let ya = self.a[i] * sa + self.xa[i] - self.a[i] * self.ya[i];
            self.xa[i] = sa;
            self.ya[i] = ya;
            sa = ya;
            let yb = self.b[i] * sb + self.xb[i] - self.b[i] * self.yb[i];
            self.xb[i] = sb;
            self.yb[i] = yb;
            sb = yb;
        }
        (sa, sb)
    }
}

impl InsertFx for FreqShifter {
    fn render(&mut self, io: &mut [f32]) {
        if self.shift_hz.abs() < 0.01 {
            return;
        }
        let inc = self.shift_hz / self.sr;
        for slot in io.iter_mut() {
            let x = *slot;
            let (sa, sb) = self.allpass(x);
            let d = TAU * self.rot;
            // Analytic signal = A + jB (B lags A by 90 deg); rotate by d and
            // take the real part: up for positive shift, no 2f image.
            *slot = x + (sa * d.cos() - sb * d.sin() - x) * self.mix;
            self.rot += inc;
            if self.rot >= 1.0 {
                self.rot -= 1.0;
            } else if self.rot <= -1.0 {
                self.rot += 1.0;
            }
        }
        for st in self.ya.iter_mut().chain(self.yb.iter_mut()) {
            if st.abs() < 1e-20 {
                *st = 0.0;
            }
        }
    }

    fn set_param(&mut self, param: ParamId, value: f32) {
        use crate::params::*;
        match param {
            SHIFT_HZ => self.shift_hz = value.clamp(-5000.0, 5000.0),
            SHIFT_MIX => self.mix = value.clamp(0.0, 1.0),
            _ => {}
        }
    }

    fn reset(&mut self) {
        self.xa = [0.0; STAGES];
        self.ya = [0.0; STAGES];
        self.xb = [0.0; STAGES];
        self.yb = [0.0; STAGES];
        self.rot = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fill_sine(buf: &mut [f32], hz: f32, block: usize) {
        for (i, slot) in buf.iter_mut().enumerate() {
            *slot = 0.5 * (TAU * hz * ((block * 4800 + i) as f32) / 48_000.0).sin();
        }
    }

    #[test]
    fn shift_is_audible_as_crossing_rate_change() {
        let run = |shift: f32| {
            let mut s = FreqShifter::new(48_000);
            s.set_param(crate::params::SHIFT_HZ, shift);
            let mut crossings = 0.0f32;
            let mut prev = 0.0f32;
            let mut buf = [0.0f32; 4800];
            for block in 0..10 {
                fill_sine(&mut buf, 440.0, block);
                s.render(&mut buf);
                for &v in &buf[480..] {
                    if v >= 0.0 && prev < 0.0 {
                        crossings += 1.0;
                    }
                    prev = v;
                    assert!(v.is_finite());
                }
            }
            crossings / 0.9
        };
        let dry = run(0.0);
        let up = run(110.0);
        let down = run(-110.0);
        // Rising crossings over 47_520 of 48_000 rendered frames: 440 Hz in
        // -> ~440/s; shifted -> 440±110.
        assert!((dry - 440.0).abs() < 44.0, "dry rate {dry}");
        assert!((up - 550.0).abs() < 60.0, "up-shift rate {up}");
        assert!((down - 330.0).abs() < 60.0, "down-shift rate {down}");
    }

    #[test]
    fn silence_in_is_silence_out_and_long_run_stays_clean() {
        let mut s = FreqShifter::new(48_000);
        s.set_param(crate::params::SHIFT_HZ, 333.0);
        let mut quiet = [0.0f32; 4800];
        s.render(&mut quiet);
        assert!(quiet.iter().all(|&v| v == 0.0), "silence in must be silence out");

        s.set_param(crate::params::SHIFT_MIX, 1.0);
        let mut buf = [0.0f32; 4800];
        for block in 0..100 {
            fill_sine(&mut buf, 440.0, block);
            s.render(&mut buf);
            let peak = buf.iter().fold(0.0f32, |m, &v| m.max(v.abs()));
            assert!(buf.iter().all(|v| v.is_finite()));
            assert!(peak < 4.0, "shifter gain out of bounds: {peak}");
        }
    }
}
