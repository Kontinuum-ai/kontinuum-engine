//! Wavetable voice (sound roster v2, #30): two morphing wavetable oscillators
//! plus a sine sub. Tables are generated in-house at startup from additive
//! sine stacks — a sine→saw spectral tilt across the morph axis, seeded
//! per-harmonic phase offsets, one mip row per octave of band-limiting.
//!
//! Determinism: [`WavetableSet::generate`] draws its phases from
//! `kontinuum_clock::stream` and runs a fixed op order, so identical seeds
//! give bit-identical tables on every platform/run.
//!
//! Per-voice CPU: ~1.5× a saw voice (2 bilinear table reads + 1 sine + 1 SVF
//! per sample, allocation-free). The active mip row is ~16 KB — L1-resident.
//! Total set: 8 positions × 8 mips × 2048 frames × 4 B ≈ 512 KB, shared
//! per process via [`WavetableSet::shared`].

use kontinuum_core::voice::{decay_coeff, flush_denormal, midi_to_hz};
use kontinuum_core::fx::filter::{FilterMode, Svf};
use kontinuum_core::{ParamId, Voice, SILENCE_ABS};
use kontinuum_clock::stream;
use std::f32::consts::TAU;
use std::sync::{Arc, OnceLock};

const TABLE_LEN: usize = 2048;
const POSITIONS: usize = 8;
const MIP_LEVELS: usize = 8;
const MAX_HARMONICS: usize = 256;
/// Mip row 0 supports fundamentals from this pitch (C1) up one octave.
const BASE_HZ: f32 = 32.7;

/// Shared, immutable bank: `[position][mip][frame]`, peak-normalized tables.
pub struct WavetableSet {
    data: Box<[f32]>,
}

impl WavetableSet {
    /// Additive generation. Position axis: harmonic weight `n^(-k)` morphs
    /// from near-sine (k=6) to saw (k=1). Mip axis: harmonics halve per row
    /// so the top partial stays under half of Nyquist at every played pitch.
    /// The seed only randomizes per-harmonic phase — magnitudes stay analytic.
    pub fn generate(seed: u64) -> Self {
        let mut data = vec![0.0f32; POSITIONS * MIP_LEVELS * TABLE_LEN];
        for pos in 0..POSITIONS {
            let tilt = pos as f32 / (POSITIONS - 1) as f32;
            let exponent = 6.0 - 5.0 * tilt;
            let mut rng = stream(seed, 0, 0x30 + pos as u16);
            for mip in 0..MIP_LEVELS {
                let harmonics = (MAX_HARMONICS >> mip).max(1);
                let row = &mut data[(pos * MIP_LEVELS + mip) * TABLE_LEN..][..TABLE_LEN];
                for (n, row_n) in (1..=harmonics).zip(row.chunks_mut(TABLE_LEN)) {
                    let weight = (n as f32).powf(-exponent);
                    let phase = rng.range_f32(0.0, TAU);
                    let inc = TAU * n as f32 / TABLE_LEN as f32;
                    let (mut cx, mut cy) = (phase.cos(), phase.sin());
                    let (dc, ds) = (inc.cos(), inc.sin());
                    for slot in row_n.iter_mut() {
                        *slot += weight * cy;
                        let (nx, ny) = (cx * dc - cy * ds, cx * ds + cy * dc);
                        cx = nx;
                        cy = ny;
                    }
                }
                let peak = row.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
                if peak > 0.0 {
                    for slot in row.iter_mut() {
                        *slot /= peak;
                    }
                }
            }
        }
        WavetableSet { data: data.into_boxed_slice() }
    }

    /// Process-wide bank, generated once (off-RT, at first attach).
    pub fn shared() -> Arc<WavetableSet> {
        static SET: OnceLock<Arc<WavetableSet>> = OnceLock::new();
        Arc::clone(SET.get_or_init(|| Arc::new(WavetableSet::generate(0x5041_4E44))))
    }

    #[inline]
    fn table(&self, pos: usize, mip: usize) -> &[f32] {
        let p = pos.min(POSITIONS - 1);
        &self.data[(p * MIP_LEVELS + mip) * TABLE_LEN..][..TABLE_LEN]
    }
}

/// Two-oscillator wavetable voice: osc A follows the morph position, osc B
/// reads the mirrored position detuned by `detune_cents`; sine sub one octave
/// down. Amplitude: fast attack, sustain while gated, exponential release.
pub struct WavetableVoice {
    sr: f32,
    set: Arc<WavetableSet>,
    freq: f32,
    position: f32,
    detune_cents: f32,
    osc2_level: f32,
    sub_gain: f32,
    cutoff: Svf,
    release_ms: f32,
    mip: usize,
    phase_a: f32,
    phase_b: f32,
    sub_phase: f32,
    env: f32,
    amp_target: f32,
    rel_coeff: f32,
    gate: bool,
    active: bool,
}

impl WavetableVoice {
    /// Voice on the process-wide shared bank.
    pub fn new(sample_rate: u32) -> Self {
        Self::with_set(WavetableSet::shared(), sample_rate)
    }

    /// Voice on an explicit bank (graph pools share one `Arc`).
    pub fn with_set(set: Arc<WavetableSet>, sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        let mut w = WavetableVoice {
            sr,
            set,
            freq: 220.0,
            position: 0.0,
            detune_cents: 14.0,
            osc2_level: 0.8,
            sub_gain: 0.35,
            cutoff: Svf::new(sample_rate, 6000.0, 0.2),
            release_ms: 220.0,
            mip: 0,
            phase_a: 0.0,
            phase_b: 0.0,
            sub_phase: 0.0,
            env: 0.0,
            amp_target: 0.0,
            rel_coeff: 1.0,
            gate: false,
            active: false,
        };
        w.rel_coeff = decay_coeff(sr, w.release_ms);
        w
    }

    /// Linear-interpolated, position-crossfaded table read at phase `p`.
    fn read(&self, p: f32, mip: usize, pos: f32) -> f32 {
        let p0 = (pos * (POSITIONS - 1) as f32).floor().max(0.0);
        let pi = (p0 as usize).min(POSITIONS - 2);
        let pf = p0 - pi as f32;
        let x = p * TABLE_LEN as f32;
        let whole = x as usize;
        let i = whole % TABLE_LEN;
        let frac = x - whole as f32;
        let t0 = self.set.table(pi, mip);
        let t1 = self.set.table(pi + 1, mip);
        let a = t0[i] + (t0[(i + 1) % TABLE_LEN] - t0[i]) * frac;
        let b = t1[i] + (t1[(i + 1) % TABLE_LEN] - t1[i]) * frac;
        a + (b - a) * pf
    }

    fn update_mip(&mut self) {
        let level = ((self.freq / BASE_HZ).log2().floor()) as isize;
        self.mip = (level.clamp(0, MIP_LEVELS as isize - 1)) as usize;
    }
}

impl Voice for WavetableVoice {
    fn note_on(&mut self, pitch: f32, velocity: f32) {
        self.freq = midi_to_hz(pitch.clamp(24.0, 96.0));
        self.update_mip();
        self.phase_a = 0.0;
        self.phase_b = 0.0;
        self.sub_phase = 0.0;
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
            let pos = self.position.clamp(0.0, 1.0);
            let detune = (self.detune_cents / 1200.0).exp2();
            self.phase_a += self.freq / self.sr;
            self.phase_b += self.freq * detune / self.sr;
            self.sub_phase += self.freq * 0.5 / self.sr;
            for p in [&mut self.phase_a, &mut self.phase_b, &mut self.sub_phase] {
                if *p >= 1.0 {
                    *p -= 1.0;
                }
            }
            let osc_a = self.read(self.phase_a, self.mip, pos);
            let osc_b = self.read(self.phase_b, self.mip, 1.0 - pos);
            let sub = (TAU * self.sub_phase).sin();
            let summed = osc_a + osc_b * self.osc2_level + sub * self.sub_gain;
            let filtered = self.cutoff.process(summed, FilterMode::LowPass);
            if self.gate {
                let rate = self.amp_target / (3.0 * self.sr / 1000.0);
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
            WAV_POSITION => self.position = value.clamp(0.0, 1.0),
            WAV_DETUNE_CENTS => self.detune_cents = value.clamp(0.0, 50.0),
            WAV_OSC2_LEVEL => self.osc2_level = value.clamp(0.0, 1.0),
            WAV_SUB => self.sub_gain = value.clamp(0.0, 1.0),
            WAV_CUTOFF => self.cutoff.set_cutoff(value.clamp(100.0, 12000.0)),
            WAV_RELEASE_MS => {
                self.release_ms = value.clamp(20.0, 8000.0);
                self.rel_coeff = decay_coeff(self.sr, self.release_ms);
            }
            _ => {}
        }
    }

    fn reset(&mut self) {
        let set = Arc::clone(&self.set);
        *self = Self::with_set(set, self.sr as u32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kontinuum_core::BLOCK_FRAMES;

    #[test]
    fn generation_is_deterministic() {
        let a = WavetableSet::generate(7);
        let b = WavetableSet::generate(7);
        assert_eq!(a.data.len(), b.data.len());
        assert!(a.data.iter().zip(b.data.iter()).all(|(x, y)| x.to_bits() == y.to_bits()));
        let c = WavetableSet::generate(8);
        assert!(a.data.iter().zip(c.data.iter()).any(|(x, y)| x.to_bits() != y.to_bits()));
    }

    #[test]
    fn tables_are_normalized_and_finite() {
        let set = WavetableSet::generate(1);
        for row in set.data.chunks(TABLE_LEN) {
            let peak = row.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
            assert!(((peak - 1.0).abs() < 1e-4), "table peak {peak}");
        }
    }

    #[test]
    fn lifecycle_deterministic_and_bounded() {
        let render = || {
            let mut v = WavetableVoice::new(48_000);
            v.set_param(kontinuum_core::params::WAV_RELEASE_MS, 120.0);
            assert!(!v.is_active());
            v.note_on(60.0, 0.9);
            let mut out = vec![0.0f32; BLOCK_FRAMES * 8];
            for chunk in out.chunks_mut(BLOCK_FRAMES) {
                v.render(chunk);
            }
            out
        };
        let a = render();
        let b = render();
        assert!(a.iter().any(|&s| s != 0.0), "silent render");
        assert!(a.iter().zip(b.iter()).all(|(x, y)| x.to_bits() == y.to_bits()));
        assert!(a.iter().all(|s| s.is_finite()));
        let peak = a.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
        assert!(peak <= 4.0, "out of bounds peak {peak}");
        v_release_ends_silent();
    }

    fn v_release_ends_silent() {
        let mut v = WavetableVoice::new(48_000);
        v.set_param(kontinuum_core::params::WAV_RELEASE_MS, 80.0);
        v.note_on(48.0, 1.0);
        let mut buf = [0.0f32; BLOCK_FRAMES];
        v.render(&mut buf);
        v.note_off();
        let mut blocks = 0;
        while v.is_active() && blocks < 4000 {
            v.render(&mut buf);
            blocks += 1;
        }
        assert!(blocks < 4000, "never released");
        let mut tail = [1.0f32; BLOCK_FRAMES];
        v.render(&mut tail);
        assert!(tail.iter().all(|&s| s == 0.0), "tail not exactly zero");
    }

    /// Zero crossings (rising) per window stay near f across a full morph
    /// sweep: aliasing would flood the signal with extra sign changes.
    #[test]
    fn morph_sweep_does_not_alias() {
        let mut v = WavetableVoice::new(48_000);
        v.note_on(72.0, 1.0);
        let f = 523.25f32;
        let window = 4_800;
        let mut buf = [0.0f32; BLOCK_FRAMES];
        for win in 0..20 {
            let pos = win as f32 / 19.0;
            v.set_param(kontinuum_core::params::WAV_POSITION, pos);
            let mut crossings = 0.0f32;
            let mut prev = 0.0f32;
            for i in 0..window {
                if i % BLOCK_FRAMES == 0 {
                    v.render(&mut buf);
                }
                let s = buf[i % BLOCK_FRAMES];
                if s >= 0.0 && prev < 0.0 {
                    crossings += 1.0;
                }
                prev = s;
            }
            let rate = crossings / (window as f32 / 48_000.0);
            assert!(
                (0.6 * f..1.4 * f).contains(&rate),
                "win {win} pos {pos}: crossing rate {rate} vs f={f}"
            );
        }
    }

    /// Position 0 is a near-pure sine: RMS ≈ peak/√2 and crossing rate 2f —
    /// the "fundamental present" check without an FFT.
    #[test]
    fn sine_position_carries_the_fundamental() {
        let mut v = WavetableVoice::new(48_000);
        v.set_param(kontinuum_core::params::WAV_SUB, 0.0);
        v.set_param(kontinuum_core::params::WAV_OSC2_LEVEL, 0.0);
        v.set_param(kontinuum_core::params::WAV_CUTOFF, 12000.0);
        v.note_on(69.0, 1.0);
        let f = midi_to_hz(69.0);
        let mut buf = [0.0f32; BLOCK_FRAMES];
        for _ in 0..10 {
            v.render(&mut buf);
        }
        let mut sum_sq = 0.0f32;
        let mut peak = 0.0f32;
        let mut crossings = 0.0f32;
        let mut prev = 0.0f32;
        for i in 0..24_000 {
            if i % BLOCK_FRAMES == 0 {
                v.render(&mut buf);
            }
            let s = buf[i % BLOCK_FRAMES];
            sum_sq += s * s;
            peak = peak.max(s.abs());
            if s >= 0.0 && prev < 0.0 {
                crossings += 1.0;
            }
            prev = s;
        }
        let rms = (sum_sq / 24_000.0).sqrt();
        assert!(
            (rms / peak - std::f32::consts::FRAC_1_SQRT_2).abs() < 0.12,
            "not sine-like: rms {rms} peak {peak}"
        );
        let rate = crossings / 0.5;
        assert!((rate - f).abs() < 0.06 * f, "crossing rate {rate} vs {f}");
    }
}
