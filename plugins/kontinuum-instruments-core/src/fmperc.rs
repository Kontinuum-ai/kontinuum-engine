//! FM percussion voice (sound roster v2, #30): three operators plus a
//! feedback path — a carrier, two modulators, and carrier-output-to-phase
//! feedback. DX7 lineage: `ratio` is the operator frequency ratio (coarse x
//! fine in F-seconds), `index` approximates peak phase deviation in radians
//! (~4pi x normalized op-amp level), decays are exponential -60 dB times
//! standing in for DX7 envelope rates.
//!
//! Presets ([`FmPerc::set_preset`]) set inharmonic modulator ratios tuned for
//! metallic perc, toms, and bells; parameters then shape the family.
//!
//! Per-voice CPU: 3 sine evaluations + envelope math per sample — the
//! cheapest voice class in the crate, ~0.6x a kick.

use kontinuum_core::voice::{decay_coeff, flush_denormal, midi_to_hz};
use kontinuum_core::{ParamId, Voice, SILENCE_ABS};
use std::f32::consts::TAU;

/// Preset operator recipes as data: the table IS the preset vocabulary, so
/// a new family is a row, not a match arm. Ratios follow DX7-lineage
/// inharmonic stacks (3.53/8.41 = cymbal-ish sideband cluster, 1.41 = tom's
/// fifth partial, 1.4/3.16 = bell formant); `decay_scale` shapes the
/// -60 dB time per family.
pub const FM_PRESETS: [FmPresetSpec; 3] = [
    FmPresetSpec { id: FmPreset::Metallic, mod_ratio_a: 3.53, mod_ratio_b: 8.41, decay_scale: 0.8 },
    FmPresetSpec { id: FmPreset::Tom, mod_ratio_a: 1.0, mod_ratio_b: 1.41, decay_scale: 1.0 },
    FmPresetSpec { id: FmPreset::Bell, mod_ratio_a: 1.4, mod_ratio_b: 3.16, decay_scale: 2.4 },
];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FmPresetSpec {
    pub id: FmPreset,
    pub mod_ratio_a: f32,
    pub mod_ratio_b: f32,
    pub decay_scale: f32,
}

/// Preset selector (control-route values 0/1/2 via
/// `kontinuum_ir::schema::FmPercPreset`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FmPreset {
    Metallic,
    Tom,
    Bell,
}

impl FmPreset {
    fn spec(self) -> FmPresetSpec {
        FM_PRESETS
            .iter()
            .copied()
            .find(|s| s.id == self)
            .unwrap_or(FM_PRESETS[0])
    }

    fn from_id(value: f32) -> Self {
        if value < 0.5 {
            FmPreset::Metallic
        } else if value < 1.5 {
            FmPreset::Tom
        } else {
            FmPreset::Bell
        }
    }
}

pub struct FmPerc {
    sr: f32,
    freq: f32,
    ratio: f32,
    index: f32,
    feedback: f32,
    decay_ms: f32,
    preset: FmPreset,
    mod_ratio_a: f32,
    mod_ratio_b: f32,
    c_phase: f32,
    ma_phase: f32,
    mb_phase: f32,
    fb_state: f32,
    env: f32,
    env_coeff: f32,
    mod_env: f32,
    mod_coeff: f32,
    active: bool,
}

impl FmPerc {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        let mut f = FmPerc {
            sr,
            freq: 220.0,
            ratio: 1.0,
            index: 3.0,
            feedback: 0.3,
            decay_ms: 320.0,
            preset: FmPreset::Metallic,
            mod_ratio_a: 3.53,
            mod_ratio_b: 8.41,
            c_phase: 0.0,
            ma_phase: 0.0,
            mb_phase: 0.0,
            fb_state: 0.0,
            env: 0.0,
            env_coeff: 1.0,
            mod_env: 0.0,
            mod_coeff: 1.0,
            active: false,
        };
        f.update_coeffs();
        f
    }

    /// Select a preset operator recipe; ratio/index/feedback/decay params
    /// stay user-owned and keep their current values.
    pub fn set_preset(&mut self, preset: FmPreset) {
        self.preset = preset;
        let spec = preset.spec();
        self.mod_ratio_a = spec.mod_ratio_a;
        self.mod_ratio_b = spec.mod_ratio_b;
        self.update_coeffs();
    }

    fn update_coeffs(&mut self) {
        let effective = self.decay_ms * self.preset.spec().decay_scale;
        self.env_coeff = decay_coeff(self.sr, effective);
        // Modulator index falls ~4x faster: bright attack into a tonal body.
        self.mod_coeff = decay_coeff(self.sr, effective * 0.25);
    }
}

impl Voice for FmPerc {
    fn note_on(&mut self, pitch: f32, velocity: f32) {
        self.freq = midi_to_hz(pitch.clamp(24.0, 96.0));
        self.c_phase = 0.0;
        self.ma_phase = 0.0;
        self.mb_phase = 0.0;
        self.fb_state = 0.0;
        self.env = velocity.clamp(0.0, 1.0);
        self.mod_env = 1.0;
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
            let base = TAU * self.freq * self.ratio / self.sr;
            self.c_phase += base;
            self.ma_phase += base * self.mod_ratio_a;
            self.mb_phase += base * self.mod_ratio_b;
            for p in [&mut self.c_phase, &mut self.ma_phase, &mut self.mb_phase] {
                if *p >= TAU {
                    *p -= TAU;
                }
            }
            let mod_a = self.index * self.mod_env * self.ma_phase.sin();
            let mod_b = 0.5 * self.index * self.mod_env * self.mb_phase.sin();
            let carrier = (self.c_phase + mod_a + mod_b + self.fb_state).sin();
            self.fb_state = carrier * self.feedback * 2.0;
            *slot = carrier * self.env * 0.85;
            self.env = flush_denormal(self.env * self.env_coeff);
            self.mod_env = flush_denormal(self.mod_env * self.mod_coeff);
            if self.env < SILENCE_ABS {
                self.active = false;
            }
        }
    }

    fn set_param(&mut self, param: ParamId, value: f32) {
        use kontinuum_core::params::*;
        match param {
            FM_RATIO => self.ratio = value.clamp(0.25, 8.0),
            FM_INDEX => self.index = value.clamp(0.0, 8.0),
            FM_FEEDBACK => self.feedback = value.clamp(0.0, 1.0),
            FM_DECAY_MS => {
                self.decay_ms = value.clamp(20.0, 3000.0);
                self.update_coeffs();
            }
            FM_PRESET => self.set_preset(FmPreset::from_id(value)),
            _ => {}
        }
    }

    fn reset(&mut self) {
        *self = Self::new(self.sr as u32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kontinuum_core::BLOCK_FRAMES;

    fn render_note(preset: FmPreset, decay_ms: f32, pitch: f32) -> Vec<f32> {
        let mut f = FmPerc::new(48_000);
        f.set_preset(preset);
        f.set_param(kontinuum_core::params::FM_DECAY_MS, decay_ms);
        f.note_on(pitch, 1.0);
        let mut out = vec![0.0f32; 24_000];
        for chunk in out.chunks_mut(BLOCK_FRAMES) {
            f.render(chunk);
        }
        out
    }

    #[test]
    fn percussive_envelope_peaks_then_decays() {
        let out = render_note(FmPreset::Tom, 120.0, 60.0);
        let quarter = out.len() / 4;
        let peak = |s: &[f32]| s.iter().fold(0.0f32, |m, &x| m.max(x.abs()));
        let head = peak(&out[..quarter]);
        let tail = peak(&out[out.len() - quarter..]);
        assert!(head > 0.05, "silent head");
        assert!(head > tail * 8.0, "not peak-then-decay: head {head} tail {tail}");
        assert!(out.iter().all(|s| s.is_finite()));
    }

    #[test]
    fn ratio_change_is_audible_in_crossing_density() {
        let count = |out: &[f32]| out.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        let low = count(&render_note(FmPreset::Tom, 400.0, 60.0));
        let mut raised = FmPerc::new(48_000);
        raised.set_preset(FmPreset::Tom);
        raised.set_param(kontinuum_core::params::FM_RATIO, 4.0);
        raised.note_on(60.0, 1.0);
        let mut buf = vec![0.0f32; 24_000];
        for chunk in buf.chunks_mut(BLOCK_FRAMES) {
            raised.render(chunk);
        }
        let high = count(&buf);
        assert!(high > low * 2, "ratio 4 crossings {high} vs ratio 1 {low}");
    }

    #[test]
    fn lifecycle_deterministic_and_silent_tail() {
        let run = || {
            let mut f = FmPerc::new(48_000);
            f.set_preset(FmPreset::Metallic);
            f.note_on(69.0, 0.8);
            let mut out = [0.0f32; BLOCK_FRAMES];
            f.render(&mut out);
            out
        };
        let a = run();
        let b = run();
        assert!(a.iter().any(|&s| s != 0.0));
        assert!(a.iter().zip(b.iter()).all(|(x, y)| x.to_bits() == y.to_bits()));

        let mut f = FmPerc::new(48_000);
        f.note_on(60.0, 1.0);
        let mut buf = [0.0f32; BLOCK_FRAMES];
        let mut blocks = 0;
        while f.is_active() && blocks < 8000 {
            f.render(&mut buf);
            blocks += 1;
        }
        assert!(blocks < 8000, "never went idle");
        let mut tail = [1.0f32; BLOCK_FRAMES];
        f.render(&mut tail);
        assert!(tail.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn presets_differ_and_params_respect_bounds() {
        let metallic = render_note(FmPreset::Metallic, 320.0, 72.0);
        let bell = render_note(FmPreset::Bell, 800.0, 72.0);
        assert!(metallic.iter().zip(bell.iter()).any(|(x, y)| x.to_bits() != y.to_bits()));
        let mut f = FmPerc::new(48_000);
        f.set_param(kontinuum_core::params::FM_INDEX, 99.0);
        f.set_param(kontinuum_core::params::FM_FEEDBACK, -3.0);
        f.set_param(kontinuum_core::params::FM_RATIO, 1000.0);
        f.note_on(84.0, 1.0);
        let mut buf = [0.0f32; BLOCK_FRAMES * 8];
        f.render(&mut buf);
        let peak = buf.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
        assert!(peak.is_finite() && peak < 2.0, "unbounded render: {peak}");
    }
}
