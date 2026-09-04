//! FM electric piano — the deep-house chord voice. Two-operator FM: sine
//! carrier, sine modulator an octave up with a fast-decaying index, which
//! gives the tine/bell attack decaying into a warm sustained body.

use kontinuum_core::voice::{decay_coeff, flush_denormal};
use kontinuum_core::{ParamId, Voice, SILENCE_ABS};
use std::f32::consts::TAU;

pub struct Ep {
    sr: f32,
    decay_ms: f32,
    depth: f32,
    phase: f32,
    mod_phase: f32,
    mod_env: f32,
    mod_coeff: f32,
    amp_env: f32,
    amp_coeff: f32,
    freq: f32,
    velocity: f32,
    active: bool,
}

impl Ep {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        let mut e = Ep {
            sr,
            decay_ms: 1400.0,
            depth: 2.4,
            phase: 0.0,
            mod_phase: 0.0,
            mod_env: 0.0,
            mod_coeff: decay_coeff(sr, 260.0),
            amp_env: 0.0,
            amp_coeff: decay_coeff(sr, 1400.0),
            freq: 220.0,
            velocity: 0.0,
            active: false,
        };
        e.amp_coeff = decay_coeff(sr, e.decay_ms);
        e
    }
}

impl Voice for Ep {
    fn note_on(&mut self, pitch: f32, velocity: f32) {
        self.freq = 440.0 * ((pitch.clamp(24.0, 96.0) - 69.0) / 12.0).exp2();
        self.phase = 0.0;
        self.mod_phase = 0.0;
        self.mod_env = 1.0;
        self.amp_env = velocity.clamp(0.0, 1.0);
        self.velocity = self.amp_env;
        self.active = self.amp_env > 0.0;
    }

    fn note_off(&mut self) {}

    fn is_active(&self) -> bool {
        self.active
    }

    fn render(&mut self, out: &mut [f32]) {
        for slot in out.iter_mut() {
            if !self.active {
                *slot = 0.0;
                continue;
            }
            let f = self.freq;
            self.mod_phase += f * 2.0 / self.sr;
            if self.mod_phase >= 1.0 {
                self.mod_phase -= 1.0;
            }
            self.phase += f / self.sr;
            if self.phase >= 1.0 {
                self.phase -= 1.0;
            }
            let fm = (TAU * self.mod_phase).sin() * self.mod_env * self.depth;
            let mut s = (TAU * self.phase + fm).sin();
            // Slight second-harmonic warmth.
            s += 0.18 * (TAU * 2.0 * self.phase + fm * 0.7).sin();
            s *= self.amp_env * self.velocity.max(0.1);
            self.mod_env = flush_denormal(self.mod_env * self.mod_coeff);
            self.amp_env = flush_denormal(self.amp_env * self.amp_coeff);
            *slot = s * 0.8;
            if self.amp_env < SILENCE_ABS {
                self.active = false;
            }
        }
    }

    fn set_param(&mut self, param: ParamId, value: f32) {
        use kontinuum_core::params::*;
        match param {
            EP_DECAY_MS => {
                self.decay_ms = value.clamp(200.0, 6000.0);
                self.amp_coeff = decay_coeff(self.sr, self.decay_ms);
            }
            EP_DEPTH => self.depth = value.clamp(0.0, 6.0),
            _ => {}
        }
    }

    fn reset(&mut self) {
        *self = Self::new(self.sr as u32);
    }
}
