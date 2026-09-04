//! Chorus/ensemble (FX v2 subset, #30): BBD-style short delay lines whose
//! read heads are swept by slow LFOs. Two taps in quadrature phase give the
//! ensemble width; linear interpolation keeps the sweep artifact-free at
//! modulation depths of a few milliseconds. Ring buffers are allocated at
//! construction; `render` is allocation-free.
//!
//! Per-sample cost: 2 writes + 2 interpolated reads + 2 sin calls. The line
//! is 15 ms (~730 frames at 48 kHz) — L1-resident.

use super::lp_coeff;
use crate::{InsertFx, ParamId};
use std::f32::consts::{FRAC_PI_2, TAU};

const LINE_MS: f32 = 15.0;
const BASE_MS: f32 = 8.0;
const SWEEP_MS: f32 = 4.0;

pub struct Chorus {
    sr: f32,
    cap: usize,
    buf_a: Box<[f32]>,
    buf_b: Box<[f32]>,
    pos: usize,
    lfo: f32,
    rate_hz: f32,
    depth: f32,
    mix: f32,
    tone_a: f32,
    lp_a: f32,
    lp_b: f32,
}

impl Chorus {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        let cap = (((LINE_MS / 1000.0) * sr).ceil() as usize).max(4);
        Chorus {
            sr,
            cap,
            buf_a: vec![0.0; cap].into_boxed_slice(),
            buf_b: vec![0.0; cap].into_boxed_slice(),
            pos: 0,
            lfo: 0.0,
            rate_hz: 0.6,
            depth: 0.5,
            mix: 0.5,
            tone_a: lp_coeff(sr, 7500.0),
            lp_a: 0.0,
            lp_b: 0.0,
        }
    }

    /// Interpolated read `delay_ms` behind the write head at `pos`.
    fn read_tap(&self, buf: &[f32], pos: usize, delay_ms: f32) -> f32 {
        let d = (delay_ms / 1000.0 * self.sr).clamp(0.0, (self.cap - 2) as f32);
        let back = d as usize;
        let frac = d - back as f32;
        let cap = self.cap;
        let i0 = (pos + cap - back) % cap;
        let i1 = (i0 + 1) % cap;
        buf[i0] + (buf[i1] - buf[i0]) * frac
    }
}

impl InsertFx for Chorus {
    fn render(&mut self, io: &mut [f32]) {
        let sweep = self.depth * SWEEP_MS;
        for slot in io.iter_mut() {
            let x = *slot;
            self.buf_a[self.pos] = x;
            self.buf_b[self.pos] = x;
            let lfo = self.lfo * TAU;
            let delay_a = (BASE_MS - sweep) + sweep * (lfo.sin() + 1.0);
            let delay_b = (BASE_MS - sweep) + sweep * ((lfo + FRAC_PI_2).sin() + 1.0);
            let mut wa = self.read_tap(&self.buf_a, self.pos, delay_a);
            let mut wb = self.read_tap(&self.buf_b, self.pos, delay_b);
            self.lp_a += self.tone_a * (wa - self.lp_a);
            self.lp_b += self.tone_a * (wb - self.lp_b);
            wa = self.lp_a;
            wb = self.lp_b;
            *slot = x + (wa + wb) * 0.5 * self.mix;
            self.lfo += self.rate_hz / self.sr;
            if self.lfo >= 1.0 {
                self.lfo -= 1.0;
            }
            self.pos = (self.pos + 1) % self.cap;
        }
        if self.lp_a.abs() < 1e-20 {
            self.lp_a = 0.0;
        }
        if self.lp_b.abs() < 1e-20 {
            self.lp_b = 0.0;
        }
    }

    fn set_param(&mut self, param: ParamId, value: f32) {
        use crate::params::*;
        match param {
            CHORUS_RATE => self.rate_hz = value.clamp(0.05, 10.0),
            CHORUS_DEPTH => self.depth = value.clamp(0.0, 1.0),
            CHORUS_MIX => self.mix = value.clamp(0.0, 1.0),
            _ => {}
        }
    }

    fn reset(&mut self) {
        self.buf_a.fill(0.0);
        self.buf_b.fill(0.0);
        self.pos = 0;
        self.lfo = 0.0;
        self.lp_a = 0.0;
        self.lp_b = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sine_input_stays_bounded_and_silence_stays_silent() {
        let mut c = Chorus::new(48_000);
        let mut buf = vec![0.0f32; 4800];
        for (i, slot) in buf.iter_mut().enumerate() {
            *slot = 0.5 * (TAU * 1000.0 * i as f32 / 48_000.0).sin();
        }
        c.render(&mut buf);
        let peak = buf.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
        assert!(buf.iter().all(|s| s.is_finite()));
        assert!(peak < 1.6, "chorus gain out of bounds: {peak}");
        assert!(peak > 0.3, "chorus lost signal: {peak}");

        let mut quiet = [0.0f32; 4800];
        c.render(&mut quiet);
        // The 15 ms line still holds the sine's tail after one block; the
        // next block must be exactly silent.
        let mut after = [0.0f32; 4800];
        c.render(&mut after);
        assert!(after.iter().all(|&s| s == 0.0), "delay line never cleared");
    }

    #[test]
    fn long_run_no_nan_or_denormal_blowup() {
        let mut c = Chorus::new(48_000);
        c.set_param(crate::params::CHORUS_RATE, 8.0);
        c.set_param(crate::params::CHORUS_DEPTH, 1.0);
        let mut buf = [0.0f32; 4800];
        for block in 0..100 {
            for (i, slot) in buf.iter_mut().enumerate() {
                *slot = 0.3 * (TAU * 220.0 * ((block * 4800 + i) as f32) / 48_000.0).sin();
            }
            c.render(&mut buf);
            assert!(buf.iter().all(|s| s.is_finite() && s.abs() < 4.0));
        }
    }
}
