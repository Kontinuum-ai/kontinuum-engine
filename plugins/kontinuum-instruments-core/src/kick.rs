//! Analog-style kick: sine with exponential pitch drop, amplitude decay,
//! filtered noise click, tanh drive.

use kontinuum_core::voice::{decay_coeff, flush_denormal, HitJitter, HIT_VARIANTS, NoiseGen};
use kontinuum_core::{ParamId, Voice, SILENCE_ABS};
use std::f32::consts::TAU;

pub struct Kick {
    sr: f32,
    tune_hz: f32,
    note_tune_hz: f32,
    decay_ms: f32,
    click: f32,
    click_gain: f32,
    drive: f32,
    phase: f32,
    sub_phase: f32,
    env: f32,
    env_coeff: f32,
    penv: f32,
    penv_coeff: f32,
    click_env: f32,
    click_coeff: f32,
    click_hp_a: f32,
    click_lp: f32,
    noise: NoiseGen,
    jitter: HitJitter,
    active: bool,
}

impl Kick {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        let mut k = Kick {
            sr,
            tune_hz: 50.0,
            note_tune_hz: 50.0,
            sub_phase: 0.0,
            decay_ms: 430.0,
            click: 0.55,
            click_gain: 1.0,
            drive: 2.2,
            phase: 0.0,
            env: 0.0,
            env_coeff: 1.0,
            penv: 0.0,
            penv_coeff: decay_coeff(sr, 9.0),
            click_env: 0.0,
            click_coeff: decay_coeff(sr, 1.2),
            click_hp_a: 1.0 - (-TAU * 1800.0 / sr).exp(),
            click_lp: 0.0,
            noise: NoiseGen::seeded(),
            jitter: HitJitter::new(),
            active: false,
        };
        k.env_coeff = decay_coeff(sr, k.decay_ms);
        k
    }
}

impl Voice for Kick {
    fn note_on(&mut self, pitch: f32, velocity: f32) {
        let j = self.jitter.next_hit((0.78, 1.12), 18.0, 0.08, (0.8, 1.2));
        self.note_tune_hz =
            self.tune_hz * ((pitch.clamp(24.0, 96.0) - 60.0) / 12.0).exp2() * j.pitch;
        self.phase = 0.0;
        self.sub_phase = 0.0;
        self.env = velocity.clamp(0.0, 1.0) * j.amp;
        self.env_coeff = decay_coeff(self.sr, self.decay_ms * j.decay);
        self.penv = 1.0;
        self.click_env = 1.0;
        self.click_gain = j.tone;
        self.click_lp = 0.0;
        self.noise = NoiseGen::seeded_at(HIT_VARIANTS[j.variant]);
        self.active = self.env > 0.0;
    }

    fn note_off(&mut self) {
        // one-shot: gate is a no-op
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
            // Squared pitch envelope: 2x -> 1x with a soft knee.
            let freq = self.note_tune_hz * (1.0 + self.penv * self.penv);
            self.phase += freq / self.sr;
            if self.phase >= 1.0 {
                self.phase -= 1.0;
            }
            self.sub_phase += freq * 0.5 / self.sr;
            if self.sub_phase >= 1.0 {
                self.sub_phase -= 1.0;
            }
            // The sub-octave sits at half the tuning — 22–27 Hz at any usual
            // kick tuning, which is below what nearly all playback reproduces.
            // At 0.5 it was spending a large share of the mix's energy (and the
            // limiter's headroom) on something no one hears; 0.22 keeps the
            // weight it adds under the fundamental without the ballast.
            let body = (TAU * self.phase).sin() + 0.22 * (TAU * self.sub_phase).sin();
            let mut s = body * self.env;
            let click_in = self.noise.next_f32() * self.click_env * self.click * self.click_gain * 1.6;
            self.click_lp += self.click_hp_a * (click_in - self.click_lp);
            s += click_in - self.click_lp;
            s = (s * self.drive).tanh() * 0.82;
            self.env = flush_denormal(self.env * self.env_coeff);
            self.penv = flush_denormal(self.penv * self.penv_coeff);
            self.click_env = flush_denormal(self.click_env * self.click_coeff);
            *slot = s;
            if self.env < SILENCE_ABS {
                self.active = false;
            }
        }
    }

    fn set_param(&mut self, param: ParamId, value: f32) {
        use kontinuum_core::params::*;
        match param {
            KICK_TUNE_HZ => self.tune_hz = value.clamp(20.0, 200.0),
            KICK_DECAY_MS => {
                self.decay_ms = value.clamp(10.0, 2000.0);
                self.env_coeff = decay_coeff(self.sr, self.decay_ms);
            }
            KICK_CLICK => self.click = value.clamp(0.0, 1.0),
            KICK_DRIVE => self.drive = value.clamp(0.2, 8.0),
            _ => {}
        }
    }

    fn reset(&mut self) {
        *self = Self::new(self.sr as u32);
    }
}
