//! Poly-capable pad voice (mono instance; polyphony via pool): 3 detuned saws,
//! lowpass, slow attack / exponential release. Retriggerable with fixed initial
//! phase for bit-stable renders.

use kontinuum_core::voice::{decay_coeff, flush_denormal, midi_to_hz};
use kontinuum_core::fx::filter::{FilterMode, Svf};
use kontinuum_core::{ParamId, Voice, SILENCE_ABS};

pub struct Pad {
    sr: f32,
    detune_cents: f32,
    cutoff_hz: f32,
    attack_ms: f32,
    release_ms: f32,
    freq: f32,
    phases: [f32; 3],
    filter: Svf,
    env: f32,
    amp_target: f32,
    rel_coeff: f32,
    gate: bool,
    active: bool,
}

impl Pad {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        let mut p = Pad {
            sr,
            detune_cents: 12.0,
            cutoff_hz: 2200.0,
            attack_ms: 250.0,
            release_ms: 800.0,
            freq: 0.0,
            phases: [0.0; 3],
            filter: Svf::new(sample_rate, 2200.0, 0.2),
            env: 0.0,
            amp_target: 0.0,
            rel_coeff: 1.0,
            gate: false,
            active: false,
        };
        p.rel_coeff = decay_coeff(sr, p.release_ms);
        p
    }
}

impl Voice for Pad {
    fn note_on(&mut self, pitch: f32, velocity: f32) {
        self.freq = midi_to_hz(pitch.clamp(24.0, 96.0));
        self.phases = [0.0; 3];
        self.amp_target = velocity.clamp(0.0, 1.0);
        self.env = 0.0;
        self.gate = true;
        self.active = true;
    }

    fn note_off(&mut self) {
        self.gate = false;
    }

    fn is_active(&self) -> bool {
        self.active
    }

    fn render(&mut self, out: &mut [f32]) {
        for slot in out.iter_mut() {
            if !self.active {
                *slot = 0.0;
                continue;
            }
            let mut sum = 0.0f32;
            for i in 0..3 {
                let f = self.freq * (((i as f32) - 1.0) * self.detune_cents / 1200.0).exp2();
                self.phases[i] += f / self.sr;
                if self.phases[i] >= 1.0 {
                    self.phases[i] -= 1.0;
                }
                sum += 1.0 - 2.0 * self.phases[i];
            }
            sum *= 0.33;
            let filtered = self.filter.process(sum, FilterMode::LowPass);
            if self.gate {
                let rate = self.amp_target / (self.attack_ms.max(0.5) * self.sr / 1000.0);
                self.env = (self.env + rate).min(self.amp_target);
            } else {
                self.env = flush_denormal(self.env * self.rel_coeff);
            }
            *slot = filtered * self.env;
            if !self.gate && self.env < SILENCE_ABS {
                self.active = false;
            }
        }
    }

    fn set_param(&mut self, param: ParamId, value: f32) {
        use kontinuum_core::params::*;
        match param {
            PAD_DETUNE_CENTS => self.detune_cents = value.clamp(0.0, 50.0),
            PAD_CUTOFF => {
                self.cutoff_hz = value.clamp(100.0, 8000.0);
                self.filter.set_cutoff(self.cutoff_hz);
            }
            PAD_ATTACK_MS => self.attack_ms = value.clamp(1.0, 5000.0),
            PAD_RELEASE_MS => {
                self.release_ms = value.clamp(20.0, 8000.0);
                self.rel_coeff = decay_coeff(self.sr, self.release_ms);
            }
            _ => {}
        }
    }

    fn reset(&mut self) {
        *self = Self::new(self.sr as u32);
    }
}
