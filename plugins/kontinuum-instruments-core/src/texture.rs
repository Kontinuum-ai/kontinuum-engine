//! Noise/texture generator (sound roster v2, #30). Two gated modes:
//! - bed: granulated white noise — raised-cosine grains retriggered at a
//!   fixed rate, per-grain gain drawn from a fixed-seed stream;
//! - crackle: vinyl/tape surface noise — sparse seeded impulses (density =
//!   probability per frame), amplitude cubed for a "mostly dust, occasional
//!   pop" distribution, then a one-pole lowpass standing in for groove wear.
//!
//! Determinism: `note_on` reseeds the noise stream, so identical notes are
//! bit-identical. Release is a fixed 150 ms fade; the voice hard-mutes below
//! [`kontinuum_core::SILENCE_ABS`].
//!
//! Per-voice CPU: 1 rng draw + 1 one-pole filter per sample — comparable to
//! the hat voice.

use kontinuum_core::voice::{decay_coeff, flush_denormal, NoiseGen};
use kontinuum_core::{ParamId, Voice, SILENCE_ABS};

const RELEASE_MS: f32 = 150.0;

pub struct Texture {
    sr: f32,
    crackle: bool,
    density: f32,
    grain_len: usize,
    tone: f32,
    lp_a: f32,
    lp: f32,
    grain_pos: usize,
    grain_gain: f32,
    noise: NoiseGen,
    env: f32,
    rel_coeff: f32,
    gate: bool,
    active: bool,
}

impl Texture {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        let mut t = Texture {
            sr,
            crackle: false,
            density: 0.002,
            grain_len: (0.03 * sr) as usize,
            tone: 0.5,
            lp_a: 0.0,
            lp: 0.0,
            grain_pos: 0,
            grain_gain: 1.0,
            noise: NoiseGen::seeded(),
            env: 0.0,
            rel_coeff: 1.0,
            gate: false,
            active: false,
        };
        t.update_coeffs();
        t.rel_coeff = decay_coeff(sr, RELEASE_MS);
        t
    }

    fn update_coeffs(&mut self) {
        let cutoff = if self.crackle {
            800.0 + self.tone.clamp(0.0, 1.0) * 6000.0
        } else {
            400.0 + self.tone.clamp(0.0, 1.0) * 6000.0
        };
        self.lp_a = 1.0 - (-std::f32::consts::TAU * cutoff / self.sr).exp();
    }
}

impl Voice for Texture {
    fn note_on(&mut self, _pitch: f32, velocity: f32) {
        self.noise = NoiseGen::seeded();
        self.lp = 0.0;
        self.grain_pos = 0;
        self.grain_gain = 1.0;
        self.env = velocity.clamp(0.0, 1.0);
        self.gate = true;
        self.active = self.env > 0.0;
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
            let src = if self.crackle {
                let draw = self.noise.next_f32() * 0.5 + 0.5;
                if draw < self.density.clamp(0.0, 0.05) {
                    let v = self.noise.range_f32(-1.0, 1.0);
                    v * v * v * 0.9
                } else {
                    self.noise.next_f32() * 0.01
                }
            } else {
                if self.grain_pos >= self.grain_len {
                    self.grain_pos = 0;
                    self.grain_gain = 0.35 + 0.65 * (self.noise.next_f32() * 0.5 + 0.5);
                }
                self.grain_pos += 1;
                let x = self.grain_pos as f32 / self.grain_len.max(1) as f32;
                let window = 0.5 * (1.0 - (std::f32::consts::PI * x).cos());
                self.noise.next_f32() * window * self.grain_gain
            };
            self.lp += self.lp_a * (src - self.lp);
            *slot = self.lp * self.env;
            if !self.gate {
                self.env = flush_denormal(self.env * self.rel_coeff);
                if self.env < SILENCE_ABS {
                    self.active = false;
                }
            }
        }
    }

    fn set_param(&mut self, param: ParamId, value: f32) {
        use kontinuum_core::params::*;
        match param {
            TEX_MODE => {
                self.crackle = value >= 0.5;
                self.update_coeffs();
            }
            TEX_DENSITY => self.density = value.clamp(0.0, 0.05),
            TEX_GRAIN_MS => {
                self.grain_len = ((value.clamp(2.0, 200.0) / 1000.0) * self.sr) as usize;
            }
            TEX_TONE => {
                self.tone = value.clamp(0.0, 1.0);
                self.update_coeffs();
            }
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
    use kontinuum_core::params::{TEX_DENSITY, TEX_GRAIN_MS, TEX_MODE, TEX_TONE};

    fn render_gated(t: &mut Texture, frames: usize) -> Vec<f32> {
        let mut out = vec![0.0f32; frames];
        let mut done = 0;
        while done < frames {
            let n = BLOCK_FRAMES.min(frames - done);
            t.render(&mut out[done..done + n]);
            done += n;
        }
        out
    }

    #[test]
    fn bed_is_non_silent_finite_and_deterministic() {
        let render = || {
            let mut t = Texture::new(48_000);
            t.set_param(TEX_MODE, 0.0);
            t.note_on(60.0, 0.8);
            render_gated(&mut t, BLOCK_FRAMES * 8)
        };
        let a = render();
        let b = render();
        assert!(a.iter().any(|&s| s != 0.0));
        assert!(a.iter().all(|s| s.is_finite()));
        assert!(a.iter().zip(b.iter()).all(|(x, y)| x.to_bits() == y.to_bits()));
    }

    #[test]
    fn crackle_impulse_count_tracks_density() {
        let count_pops = |density: f32| {
            let mut t = Texture::new(48_000);
            t.set_param(TEX_MODE, 1.0);
            t.set_param(TEX_DENSITY, density);
            t.note_on(60.0, 1.0);
            let out = render_gated(&mut t, 48_000);
            let mut runs = 0;
            let mut inside = false;
            for &s in &out {
                if s.abs() > 0.06 {
                    if !inside {
                        runs += 1;
                        inside = true;
                    }
                } else {
                    inside = false;
                }
            }
            runs
        };
        let sparse = count_pops(0.001);
        let dense = count_pops(0.01);
        assert!((20..=350).contains(&sparse), "sparse crackle runs {sparse}");
        assert!(dense > sparse * 2, "density not audible: {dense} vs {sparse}");
    }

    #[test]
    fn release_ends_in_exact_silence_and_stays_finite_long() {
        let mut t = Texture::new(48_000);
        t.set_param(TEX_TONE, 0.9);
        t.set_param(TEX_GRAIN_MS, 8.0);
        t.note_on(60.0, 1.0);
        let mut buf = [0.0f32; BLOCK_FRAMES];
        t.render(&mut buf);
        t.note_off();
        let mut blocks = 0;
        while t.is_active() && blocks < 4000 {
            t.render(&mut buf);
            blocks += 1;
        }
        assert!(blocks < 4000);
        let mut tail = [1.0f32; BLOCK_FRAMES];
        t.render(&mut tail);
        assert!(tail.iter().all(|&s| s == 0.0));

        // Long-run stability: 10 s crackle stays finite, no denormal lockup.
        t.note_on(60.0, 1.0);
        let mut out = [0.0f32; BLOCK_FRAMES];
        let mut active = true;
        for _ in 0..7_500 {
            if active {
                t.render(&mut out);
                active = t.is_active();
            } else {
                out.fill(0.0);
            }
            assert!(out.iter().all(|s| s.is_finite()));
        }
    }
}
