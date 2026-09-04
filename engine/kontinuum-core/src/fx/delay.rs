//! Feedback delay. Wet-only output: the bus path adds wet to the mix while the
//! dry signal reaches it via the direct track path. Ring buffers allocated at
//! construction (capacity = 1 s).
//!
//! Tape mode (#30): wow/flutter modulate the read position inside the loop
//! (fractional delay, linear interpolation) and `TAPE_SAT` drives a tanh
//! shaper on the recirculated signal — the classic darkening loop of a tape
//! echo. All three params at 0 keep the clean path bit-exact with the
//! pre-tape render (integer read, no shaper), so existing sessions are
//! untouched.
//!
//! Per-sample cost: clean ~4 flops + 1 one-pole; tape adds 2 lerp reads, 3
//! sin and 2 tanh (the wow LFO is per-sample — wow is a sub-Hz drift, so a
//! block-rate coefficient would step audibly on long delays).

use super::lp_coeff;
use crate::{BusFx, ParamId};
use std::f32::consts::TAU;

pub struct Delay {
    cap: usize,
    buf_l: Box<[f32]>,
    buf_r: Box<[f32]>,
    pos: usize,
    delay_frames: usize,
    feedback: f32,
    mix: f32,
    tone_a: f32,
    lp_l: f32,
    lp_r: f32,
    wow: f32,
    flutter: f32,
    sat: f32,
    wow_phase: f32,
    flutter_phase: f32,
}

impl Delay {
    pub fn new(sample_rate: u32) -> Self {
        let cap = (sample_rate as usize).max(64);
        Delay {
            cap,
            buf_l: vec![0.0; cap].into_boxed_slice(),
            buf_r: vec![0.0; cap].into_boxed_slice(),
            pos: 0,
            delay_frames: (sample_rate as usize / 4).clamp(1, cap - 1),
            feedback: 0.45,
            mix: 0.5,
            tone_a: lp_coeff(sample_rate as f32, 6000.0),
            lp_l: 0.0,
            lp_r: 0.0,
            wow: 0.0,
            flutter: 0.0,
            sat: 0.0,
            wow_phase: 0.0,
            flutter_phase: 0.0,
        }
    }

    fn tape_active(&self) -> bool {
        self.wow > 0.0 || self.flutter > 0.0 || self.sat > 0.0
    }

    /// Loop-read position offset in frames. Wow is a slow pitch sag
    /// (±0.35% of the delay at 0.5 Hz); flutter is a faster, slightly
    /// irregular modulation (7.3 Hz + 11.1 Hz, ±0.18%).
    fn wow_flutter_frames(&mut self, delay: f32) -> f32 {
        let t = TAU * self.wow_phase;
        let flutter_t = TAU * self.flutter_phase;
        let mut d = 0.0035 * self.wow * t.sin()
            + 0.0013 * self.flutter * (flutter_t.sin() + 0.5 * (2.0 * flutter_t + 1.3).sin());
        if d.abs() < 1e-9 {
            d = 0.0;
        }
        self.wow_phase += 0.5 / self.cap as f32;
        if self.wow_phase >= 1.0 {
            self.wow_phase -= 1.0;
        }
        self.flutter_phase += 7.3 / self.cap as f32;
        if self.flutter_phase >= 1.0 {
            self.flutter_phase -= 1.0;
        }
        delay * d
    }

    /// tanh soft-clip on the recirculated signal: small-signal slope stays
    /// unity (the loop cannot gain energy from the shaper) and peaks round
    /// off — the compression side of tape, not an expander.
    fn saturate(&self, x: f32) -> f32 {
        if self.sat <= 0.0 {
            return x;
        }
        let g = 1.0 + 3.0 * self.sat;
        (g * x).tanh() / g
    }

    /// Interpolated stereo read `when` frames behind the write head.
    fn read_frac(&self, when: f32) -> (f32, f32) {
        let back = when.clamp(1.0, (self.cap - 2) as f32);
        let whole = back as usize;
        let frac = back - whole as f32;
        let i0 = (self.pos + self.cap - whole) % self.cap;
        let i1 = (i0 + 1) % self.cap;
        let l = self.buf_l[i0] + (self.buf_l[i1] - self.buf_l[i0]) * frac;
        let r = self.buf_r[i0] + (self.buf_r[i1] - self.buf_r[i0]) * frac;
        (l, r)
    }

    pub fn set_delay_frames(&mut self, frames: usize) {
        self.delay_frames = frames.clamp(1, self.cap - 1);
    }

    pub fn set_feedback(&mut self, fb: f32) {
        self.feedback = fb.clamp(0.0, 0.95);
    }

    pub fn set_mix(&mut self, mix: f32) {
        self.mix = mix.clamp(0.0, 1.0);
    }

    pub fn set_tone_cutoff(&mut self, sample_rate: f32, cutoff_hz: f32) {
        self.tone_a = lp_coeff(sample_rate, cutoff_hz);
    }
}

impl BusFx for Delay {
    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        if self.tape_active() {
            self.render_tape(left, right);
        } else {
            self.render_clean(left, right);
        }
    }

    fn set_param(&mut self, param: ParamId, value: f32) {
        use crate::params::*;
        match param {
            DELAY_TIME_FRAMES => self.set_delay_frames(value.max(1.0) as usize),
            DELAY_FEEDBACK => self.set_feedback(value),
            DELAY_MIX => self.set_mix(value),
            DELAY_TONE => self.set_tone_cutoff(48_000.0, 800.0 + value.clamp(0.0, 1.0) * 11_200.0),
            TAPE_WOW => self.wow = value.clamp(0.0, 1.0),
            TAPE_FLUTTER => self.flutter = value.clamp(0.0, 1.0),
            TAPE_SAT => self.sat = value.clamp(0.0, 1.0),
            _ => {}
        }
    }

    fn reset(&mut self) {
        self.buf_l.fill(0.0);
        self.buf_r.fill(0.0);
        self.pos = 0;
        self.lp_l = 0.0;
        self.lp_r = 0.0;
        self.delay_frames = (self.cap / 4).clamp(1, self.cap - 1);
        self.feedback = 0.45;
        self.mix = 0.5;
        self.tone_a = lp_coeff(self.cap as f32, 6000.0);
        self.wow = 0.0;
        self.flutter = 0.0;
        self.sat = 0.0;
        self.wow_phase = 0.0;
        self.flutter_phase = 0.0;
    }
}

impl Delay {
    /// The pre-#30 loop, kept verbatim so clean renders stay bit-exact.
    fn render_clean(&mut self, left: &mut [f32], right: &mut [f32]) {
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let read = (self.pos + self.cap - self.delay_frames) % self.cap;
            let mut wl = self.buf_l[read];
            let mut wr = self.buf_r[read];
            self.lp_l += self.tone_a * (wl - self.lp_l);
            wl = self.lp_l;
            self.lp_r += self.tone_a * (wr - self.lp_r);
            wr = self.lp_r;
            if self.lp_l.abs() < 1e-20 {
                self.lp_l = 0.0;
            }
            if self.lp_r.abs() < 1e-20 {
                self.lp_r = 0.0;
            }
            self.buf_l[self.pos] = *l + wl * self.feedback;
            self.buf_r[self.pos] = *r + wr * self.feedback;
            *l = wl * self.mix;
            *r = wr * self.mix;
            self.pos = (self.pos + 1) % self.cap;
        }
    }

    /// Tape loop: fractional read with wow/flutter position modulation,
    /// tanh saturation on the recirculated write, tone LP as before.
    fn render_tape(&mut self, left: &mut [f32], right: &mut [f32]) {
        let base = self.delay_frames as f32;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let when = base + self.wow_flutter_frames(base);
            let (mut wl, mut wr) = self.read_frac(when);
            self.lp_l += self.tone_a * (wl - self.lp_l);
            wl = self.lp_l;
            self.lp_r += self.tone_a * (wr - self.lp_r);
            wr = self.lp_r;
            wl = self.saturate(wl);
            wr = self.saturate(wr);
            if wl.abs() < 1e-20 {
                wl = 0.0;
            }
            if wr.abs() < 1e-20 {
                wr = 0.0;
            }
            self.buf_l[self.pos] = *l + wl * self.feedback;
            self.buf_r[self.pos] = *r + wr * self.feedback;
            *l = wl * self.mix;
            *r = wr * self.mix;
            self.pos = (self.pos + 1) % self.cap;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn impulse_tail_decays_and_is_finite() {
        let mut delay = Delay::new(48_000);
        delay.set_delay_frames(240);
        delay.set_feedback(0.6);
        delay.set_mix(1.0);
        let mut l = vec![0.0f32; 4800];
        let mut r = vec![0.0f32; 4800];
        l[0] = 1.0;
        r[0] = 1.0;
        delay.render(&mut l, &mut r);
        assert!(l.iter().all(|s| s.is_finite()));
        let head: f32 = l[..1000].iter().map(|s| s * s).sum();
        let tail: f32 = l[3800..].iter().map(|s| s * s).sum();
        assert!(head > tail * 100.0, "delay tail not decaying: head {head} tail {tail}");
    }

    #[test]
    fn tape_mode_default_off_keeps_clean_render_bit_exact() {
        let run = |tape: bool| {
            let mut d = Delay::new(48_000);
            d.set_delay_frames(240);
            d.set_feedback(0.6);
            d.set_mix(1.0);
            if tape {
                d.set_param(crate::params::TAPE_WOW, 0.8);
                d.set_param(crate::params::TAPE_FLUTTER, 0.6);
                d.set_param(crate::params::TAPE_SAT, 0.5);
            }
            let mut l = vec![0.0f32; 9600];
            let mut r = vec![0.0f32; 9600];
            for (i, (l, r)) in l.iter_mut().zip(r.iter_mut()).enumerate() {
                let s = 0.6 * (TAU * 220.0 * i as f32 / 48_000.0).sin();
                *l = s;
                *r = s;
            }
            d.render(&mut l, &mut r);
            (l, r)
        };
        let clean_a = run(false);
        let clean_b = run(false);
        assert!(
            clean_a.0.iter().zip(clean_b.0.iter()).all(|(x, y)| x.to_bits() == y.to_bits()),
            "clean render not deterministic"
        );
        let tape = run(true);
        assert!(
            clean_a.0.iter().zip(tape.0.iter()).any(|(x, y)| x.to_bits() != y.to_bits()),
            "tape params changed nothing"
        );
        assert!(tape.0.iter().all(|s| s.is_finite()));
        let peak = tape.0.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
        assert!(peak < 4.0, "tape loop out of bounds: {peak}");
    }

    #[test]
    fn tape_saturated_loop_decays_and_ends_silent_after_reset() {
        let mut d = Delay::new(48_000);
        d.set_delay_frames(480);
        d.set_feedback(0.9);
        d.set_mix(1.0);
        d.set_param(crate::params::TAPE_SAT, 1.0);
        d.set_param(crate::params::TAPE_WOW, 1.0);
        let mut l = vec![0.0f32; 96_000];
        let mut r = vec![0.0f32; 96_000];
        l[0] = 1.0;
        r[0] = 1.0;
        d.render(&mut l, &mut r);
        assert!(l.iter().all(|s| s.is_finite()));
        let head: f32 = l[..1000].iter().map(|s| s * s).sum();
        let tail: f32 = l[80_000..].iter().map(|s| s * s).sum();
        assert!(head > tail * 100.0, "tape loop did not decay: head {head} tail {tail}");

        d.reset();
        let mut after_l = vec![0.0f32; 4800];
        let mut after_r = vec![0.0f32; 4800];
        d.render(&mut after_l, &mut after_r);
        assert!(after_l.iter().all(|&s| s == 0.0), "reset left loop energy");
    }
}
