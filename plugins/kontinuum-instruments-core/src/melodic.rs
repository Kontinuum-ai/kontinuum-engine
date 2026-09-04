//! Melodic machine voices: acid (303-style), pluck (Karplus-Strong), and
//! stab (detuned-saw chord hit). Deterministic, self-muting.

use kontinuum_core::voice::{decay_coeff, flush_denormal, midi_to_hz};
use kontinuum_core::fx::filter::{FilterMode, Svf};
use kontinuum_core::{ParamId, Voice, SILENCE_ABS};

/// 303-style acid bass: saw through a resonant lowpass with a pronounced
/// envelope, portamento, and per-hit accent (velocity drives the filter).
pub struct Acid {
    sr: f32,
    cutoff_hz: f32,
    env_amt: f32,
    glide_ms: f32,
    phase: f32,
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

impl Acid {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        let mut a = Acid {
            sr,
            cutoff_hz: 700.0,
            env_amt: 2.6,
            glide_ms: 60.0,
            phase: 0.0,
            freq: 0.0,
            freq_target: 0.0,
            glide_coeff: 1.0,
            filter: Svf::new(sample_rate, 700.0, 0.85),
            filter_env: 0.0,
            fe_coeff: decay_coeff(sr, 220.0),
            amp: 0.0,
            amp_target: 0.0,
            atk_coeff: 1.0,
            rel_coeff: decay_coeff(sr, 40.0),
            gate: false,
            active: false,
        };
        a.update_coeffs();
        a
    }

    fn update_coeffs(&mut self) {
        self.atk_coeff = 1.0 - decay_coeff(self.sr, 2.0);
        self.rel_coeff = decay_coeff(self.sr, 40.0);
        self.glide_coeff = 1.0 - decay_coeff(self.sr, self.glide_ms.max(0.5));
    }
}

impl Voice for Acid {
    fn note_on(&mut self, pitch: f32, velocity: f32) {
        self.freq_target = midi_to_hz(pitch.clamp(12.0, 108.0));
        if !self.active {
            self.phase = 0.0;
            self.freq = self.freq_target;
            self.amp = 0.0;
        }
        // Accent: velocity opens the filter beyond the fixed envelope.
        self.filter_env = 0.7 + velocity.clamp(0.0, 1.0) * 0.6;
        self.amp_target = velocity.clamp(0.0, 1.0);
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
            let osc = 1.0 - 2.0 * self.phase;
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
            *slot = filtered * self.amp * 1.4;
            if !self.gate && self.amp < SILENCE_ABS {
                self.active = false;
            }
        }
    }

    fn set_param(&mut self, param: ParamId, value: f32) {
        use kontinuum_core::params::*;
        match param {
            ACID_CUTOFF => {
                self.cutoff_hz = value.clamp(60.0, 8000.0);
                self.filter.set_cutoff(self.cutoff_hz);
            }
            ACID_RESONANCE => self.filter.set_resonance(value.clamp(0.0, 1.0)),
            ACID_ENV_AMT => self.env_amt = value.clamp(0.0, 4.0),
            ACID_GLIDE_MS => {
                self.glide_ms = value.clamp(0.0, 500.0);
                self.update_coeffs();
            }
            _ => {}
        }
    }

    fn reset(&mut self) {
        *self = Self::new(self.sr as u32);
    }
}

/// Karplus-Strong pluck: noise-seeded delay line through a damping lowpass —
/// a physical-model string with a natural decay.
pub struct Pluck {
    sr: f32,
    damping: f32,
    bright: f32,
    freq: f32,
    buffer: Box<[f32]>,
    pos: usize,
    lp: f32,
    lp_coeff: f32,
    amp: f32,
    active: bool,
}

impl Pluck {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        let mut p = Pluck {
            sr,
            damping: 0.55,
            bright: 0.5,
            freq: 220.0,
            buffer: vec![0.0; 64].into_boxed_slice(),
            pos: 0,
            lp: 0.0,
            lp_coeff: 0.5,
            amp: 0.0,
            active: false,
        };
        p.update_coeffs();
        p
    }

    fn update_coeffs(&mut self) {
        // Damping 0..1 maps to the loop lowpass cutoff 1200..9000 Hz.
        let cutoff = 1200.0 + self.damping.clamp(0.0, 1.0) * 7800.0;
        self.lp_coeff = 1.0 - (-std::f32::consts::TAU * cutoff.min(self.sr * 0.45) / self.sr).exp();
    }
}

impl Voice for Pluck {
    fn note_on(&mut self, pitch: f32, velocity: f32) {
        self.freq = midi_to_hz(pitch.clamp(24.0, 96.0));
        let len = ((self.sr / self.freq) as usize).clamp(8, self.buffer.len());
        self.buffer[..len].fill(0.0);
        // Deterministic excitation: fixed-seed noise burst shaped by `bright`.
        let mut n = kontinuum_core::voice::NoiseGen::seeded();
        for i in 0..len {
            let raw = n.next_f32();
            self.lp += self.lp_coeff * (raw - self.lp);
            self.buffer[i] = raw * self.bright.clamp(0.0, 1.0) + self.lp * (1.0 - self.bright.clamp(0.0, 1.0));
        }
        self.pos = 0;
        self.amp = velocity.clamp(0.0, 1.0);
        self.active = self.amp > 0.0;
    }

    fn note_off(&mut self) {}

    fn is_active(&self) -> bool {
        self.active
    }

    fn render(&mut self, out: &mut [f32]) {
        let len = self.buffer.len();
        for slot in out.iter_mut() {
            if !self.active {
                *slot = 0.0;
                continue;
            }
            let next = (self.pos + 1) % len;
            let s = self.buffer[self.pos];
            self.lp += self.lp_coeff * (s - self.lp);
            self.buffer[next] = (s + self.lp) * 0.5 * 0.996;
            self.pos = next;
            self.amp = flush_denormal(self.amp * 0.99995);
            let v = s * self.amp;
            *slot = if v.abs() < SILENCE_ABS { 0.0 } else { v };
            if self.amp < SILENCE_ABS {
                self.active = false;
            }
        }
    }

    fn set_param(&mut self, param: ParamId, value: f32) {
        use kontinuum_core::params::*;
        match param {
            PLUCK_DAMPING => {
                self.damping = value.clamp(0.0, 1.0);
                self.update_coeffs();
            }
            PLUCK_BRIGHT => self.bright = value.clamp(0.0, 1.0),
            _ => {}
        }
    }

    fn reset(&mut self) {
        self.buffer.fill(0.0);
        self.pos = 0;
        self.amp = 0.0;
        self.active = false;
    }
}

/// Stab: a tight detuned-saw chord hit — the deep-house "dub stab" when fed
/// through the delay send.
pub struct Stab {
    sr: f32,
    cutoff_hz: f32,
    decay_ms: f32,
    detune_cents: f32,
    phases: [f32; 4],
    filter: Svf,
    env: f32,
    env_coeff: f32,
    freq: f32,
    velocity: f32,
    active: bool,
}

impl Stab {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        let mut st = Stab {
            sr,
            cutoff_hz: 2600.0,
            decay_ms: 420.0,
            detune_cents: 11.0,
            phases: [0.0; 4],
            filter: Svf::new(sample_rate, 2600.0, 0.2),
            env: 0.0,
            env_coeff: 1.0,
            freq: 220.0,
            velocity: 0.0,
            active: false,
        };
        st.env_coeff = decay_coeff(sr, st.decay_ms);
        st
    }
}

impl Voice for Stab {
    fn note_on(&mut self, pitch: f32, velocity: f32) {
        self.freq = midi_to_hz(pitch.clamp(24.0, 96.0));
        self.phases = [0.0; 4];
        self.env = velocity.clamp(0.0, 1.0);
        self.velocity = self.env;
        self.active = self.env > 0.0;
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
            let mut sum = 0.0f32;
            for i in 0..4 {
                let f = self.freq * (((i as f32) - 1.5) * self.detune_cents / 1200.0).exp2();
                self.phases[i] += f / self.sr;
                if self.phases[i] >= 1.0 {
                    self.phases[i] -= 1.0;
                }
                sum += kontinuum_core::voice::poly_blep_saw(&mut self.phases[i], f / self.sr);
            }
            sum *= 0.25;
            let filtered = self.filter.process_lowpass(sum);
            let s = filtered * self.env * self.velocity.max(0.15);
            self.env = flush_denormal(self.env * self.env_coeff);
            *slot = s * 1.1;
            if self.env < SILENCE_ABS {
                self.active = false;
            }
        }
    }

    fn set_param(&mut self, param: ParamId, value: f32) {
        use kontinuum_core::params::*;
        match param {
            STAB_CUTOFF => {
                self.cutoff_hz = value.clamp(200.0, 12000.0);
                self.filter.set_cutoff(self.cutoff_hz);
            }
            STAB_DECAY_MS => {
                self.decay_ms = value.clamp(60.0, 2000.0);
                self.env_coeff = decay_coeff(self.sr, self.decay_ms);
            }
            STAB_DETUNE => self.detune_cents = value.clamp(0.0, 40.0),
            _ => {}
        }
    }

    fn reset(&mut self) {
        *self = Self::new(self.sr as u32);
    }
}
