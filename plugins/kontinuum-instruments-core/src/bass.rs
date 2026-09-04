//! Monophonic subtractive bass: saw/square, portamento, resonant SVF with
//! filter-envelope pluck, fast attack amp env.

use kontinuum_core::voice::{decay_coeff, flush_denormal, midi_to_hz};
use std::f32::consts::TAU;
use kontinuum_core::fx::filter::{FilterMode, Svf};
use kontinuum_core::{ParamId, Voice, SILENCE_ABS};

pub struct Bass {
    sr: f32,
    wave: f32,
    glide_ms: f32,
    cutoff_hz: f32,
    resonance: f32,
    env_amt: f32,
    attack_ms: f32,
    release_ms: f32,
    phase: f32,
    sub_phase: f32,
    freq: f32,
    freq_target: f32,
    glide_coeff: f32,
    filter: Svf,
    filter_env: f32,
    fe_coeff: f32,
    amp: f32,
    amp_target: f32,
    atk_coeff: f32,
    rel_coeff: f32,
    gate: bool,
    active: bool,
}

impl Bass {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        let mut b = Bass {
            sr,
            wave: 0.0,
            glide_ms: 0.0,
            cutoff_hz: 1200.0,
            resonance: 0.4,
            env_amt: 1.8,
            attack_ms: 2.0,
            release_ms: 45.0,
            phase: 0.0,
            sub_phase: 0.0,
            freq: 0.0,
            freq_target: 0.0,
            glide_coeff: 1.0,
            filter: Svf::new(sample_rate, 900.0, 0.35),
            filter_env: 0.0,
            fe_coeff: decay_coeff(sr, 90.0),
            amp: 0.0,
            amp_target: 0.0,
            atk_coeff: 1.0,
            rel_coeff: 0.999,
            gate: false,
            active: false,
        };
        b.update_coeffs();
        b
    }

    fn update_coeffs(&mut self) {
        self.atk_coeff = 1.0 - decay_coeff(self.sr, self.attack_ms);
        self.rel_coeff = decay_coeff(self.sr, self.release_ms);
        self.glide_coeff = if self.glide_ms <= 0.5 {
            1.0
        } else {
            1.0 - decay_coeff(self.sr, self.glide_ms)
        };
    }
}

impl Voice for Bass {
    fn note_on(&mut self, pitch: f32, velocity: f32) {
        self.freq_target = midi_to_hz(pitch.clamp(12.0, 108.0));
        if !self.active {
            self.phase = 0.0;
            self.sub_phase = 0.0;
            self.freq = self.freq_target;
            self.amp = 0.0;
        }
        self.amp_target = velocity.clamp(0.0, 1.0);
        self.filter_env = 1.0;
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
            if self.glide_coeff >= 1.0 {
                self.freq = self.freq_target;
            } else {
                self.freq += self.glide_coeff * (self.freq_target - self.freq);
            }
            self.phase += self.freq / self.sr;
            if self.phase >= 1.0 {
                self.phase -= 1.0;
            }
            self.sub_phase += self.freq * 0.5 / self.sr;
            if self.sub_phase >= 1.0 {
                self.sub_phase -= 1.0;
            }
            let upper = if self.wave < 0.5 {
                1.0 - 2.0 * self.phase
            } else if self.phase < 0.5 {
                1.0
            } else {
                -1.0
            };
            // Sub-octave sine under the upper voice: weight without mud.
            let osc = upper * 0.72 + (TAU * self.sub_phase).sin() * 0.55;
            self.filter_env = flush_denormal(self.filter_env * self.fe_coeff);
            let fc = (self.cutoff_hz * (self.env_amt.clamp(0.0, 4.0) * self.filter_env).exp2())
                .clamp(40.0, self.sr / 3.0);
            self.filter.set_cutoff(fc);
            let filtered = self.filter.process(osc, FilterMode::LowPass);
            if self.gate {
                self.amp += self.atk_coeff * (self.amp_target - self.amp);
            } else {
                self.amp = flush_denormal(self.amp * self.rel_coeff);
            }
            *slot = filtered * self.amp;
            if !self.gate && self.amp < SILENCE_ABS {
                self.active = false;
            }
        }
    }

    fn set_param(&mut self, param: ParamId, value: f32) {
        use kontinuum_core::params::*;
        match param {
            BASS_GLIDE_MS => {
                self.glide_ms = value.clamp(0.0, 500.0);
                self.update_coeffs();
            }
            BASS_CUTOFF => {
                self.cutoff_hz = value.clamp(40.0, 8000.0);
                self.filter.set_cutoff(self.cutoff_hz);
            }
            BASS_RESONANCE => {
                self.resonance = value.clamp(0.0, 1.0);
                self.filter.set_resonance(self.resonance);
            }
            BASS_WAVE => self.wave = value.clamp(0.0, 1.0),
            BASS_ENV_AMT => self.env_amt = value.clamp(0.0, 4.0),
            BASS_ATTACK_MS => {
                self.attack_ms = value.clamp(0.5, 200.0);
                self.update_coeffs();
            }
            BASS_RELEASE_MS => {
                self.release_ms = value.clamp(5.0, 2000.0);
                self.update_coeffs();
            }
            _ => {}
        }
    }

    fn reset(&mut self) {
        *self = Self::new(self.sr as u32);
    }
}
