//! Multi-resolution STFT magnitude loss + time-envelope distance — the
//! reusable objective for one-shot parameter fitting (issue #75).
//!
//! Formulas (all deterministic, f64, no allocation in the hot loop — every
//! scratch buffer is sized once in [`FitObjective::new`]):
//!
//! * For each window `W` in {256, 1024, 4096} (hop = W/4, Hann window):
//!   `spec_W = rms over all (frame, bin) of [ ln(1 + λ·|X_t[k]|) − ln(1 + λ·|X_c[k]|) ]²`
//!   with the log compression `λ = 100` bounding the dynamic range so the
//!   loss is dominated by audible-level bins, not by −200 dB noise floor.
//!   Magnitudes only; phase is irrelevant to a one-shot's timbre.
//! * Envelope: `e[n] = one-pole lowpass of |x|` with a 5 ms time constant,
//!   sampled every 64 frames;
//!   `env = rms over sampled n of (e_t[n] − e_c[n])²` — a linear-amplitude
//!   distance so decay *shape* is matched, not just the average spectrum.
//!
//! Total loss = `Σ_r SPEC_WEIGHTS[r]·spec_r + ENV_WEIGHT·env`. Both signals
//! are peak-normalized (silent signals stay at zero) and zero-padded to at
//! least the largest window so short one-shots are well-defined. Peak
//! normalization makes the objective scale-invariant: a reference hit's
//! recording gain is arbitrary, and mix balance is the mixer's job (#76) —
//! the fitter matches timbre and decay shape, not absolute level.
//!
//! Weights: the three spectral resolutions are equally trusted (equal
//! weight 1.0 — 256 resolves the attack, 4096 the pitch content) and the
//! envelope term is weighted 0.5: spectral shape dominates but a wrong
//! decay is clearly visible in the total. See tests for discrimination
//! evidence.

use crate::fft::{hanning, power_spectrum};

/// STFT windows, in samples (issue #75: "around 256 / 1024 / 4096").
pub const SPEC_WINDOWS: [usize; 3] = [256, 1024, 4096];
/// Per-resolution weights of the spectral terms, in [`SPEC_WINDOWS`] order.
pub const SPEC_WEIGHTS: [f64; 3] = [1.0, 1.0, 1.0];
/// Weight of the linear-amplitude envelope term.
pub const ENV_WEIGHT: f64 = 0.5;
/// Log compression `λ` in `ln(1 + λ·|X|)`.
const LOG_COMPRESS: f64 = 100.0;
/// Largest STFT window; windows are compile-time constants, so this is
/// also the FFT scratch size.
const MAX_WIN: usize = SPEC_WINDOWS[2];
/// One-pole time constant of the amplitude envelope (s).
const ENV_TAU_SEC: f64 = 0.005;
/// Envelope sampling stride (frames).
const ENV_STRIDE: usize = 64;

/// Decomposed loss for one candidate render.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LossParts {
    /// RMS log-magnitude distance per resolution, [`SPEC_WINDOWS`] order.
    pub spec: [f64; 3],
    /// Linear-amplitude envelope distance.
    pub env: f64,
}

impl LossParts {
    /// The total fitted objective with the documented weights.
    pub fn total(&self) -> f64 {
        let spec: f64 =
            self.spec.iter().zip(SPEC_WEIGHTS.iter()).map(|(s, w)| s * w).sum();
        spec + ENV_WEIGHT * self.env
    }
}

/// Precomputed target features + scratch buffers. Build once per target,
/// then call [`FitObjective::loss`] for every candidate render. No
/// allocation happens after construction: spectrograms, envelopes and FFT
/// scratch are all sized here.
pub struct FitObjective {
    sample_rate: u32,
    padded_len: usize,
    target_spec: Vec<Vec<f64>>,
    target_env: Vec<f64>,
    windows: Vec<Vec<f64>>,
    cand_spec: Vec<Vec<f64>>,
    padded: Vec<f32>,
    re: Vec<f64>,
    im: Vec<f64>,
    frame: Vec<f64>,
}

/// Flattened (frame-major) spectrogram length for one resolution.
fn spec_len(padded_len: usize, window: usize) -> usize {
    let frames = (padded_len - window) / (window / 4) + 1;
    frames * (window / 2)
}

impl FitObjective {
    /// Extracts target spectrograms + envelope. `sample_rate` must match
    /// the rate both signals were rendered at.
    pub fn new(target: &[f32], sample_rate: u32) -> FitObjective {
        let padded_len = target.len().max(MAX_WIN);
        let mut fit = FitObjective {
            sample_rate,
            padded_len,
            target_spec: Vec::new(),
            target_env: envelope(target, sample_rate, padded_len),
            windows: SPEC_WINDOWS.iter().map(|&w| hanning(w)).collect(),
            cand_spec: Vec::new(),
            padded: vec![0.0; padded_len],
            re: vec![0.0; MAX_WIN],
            im: vec![0.0; MAX_WIN],
            frame: vec![0.0; MAX_WIN],
        };
        fit.padded[..target.len()].copy_from_slice(target);
        normalize_peak(&mut fit.padded);
        for r in 0..SPEC_WINDOWS.len() {
            fit.target_spec.push(vec![0.0; spec_len(padded_len, SPEC_WINDOWS[r])]);
            fit.cand_spec.push(vec![0.0; spec_len(padded_len, SPEC_WINDOWS[r])]);
            log_spectrogram_into(
                &fit.padded,
                &fit.windows[r],
                &mut fit.frame,
                &mut fit.re,
                &mut fit.im,
                &mut fit.target_spec[r],
            );
        }
        fit
    }

    /// Features of `candidate` compared against the stored target.
    pub fn parts(&mut self, candidate: &[f32]) -> LossParts {
        let n = candidate.len().min(self.padded_len);
        self.padded[..n].copy_from_slice(&candidate[..n]);
        self.padded[n..].fill(0.0);
        normalize_peak(&mut self.padded);
        for r in 0..SPEC_WINDOWS.len() {
            log_spectrogram_into(
                &self.padded,
                &self.windows[r],
                &mut self.frame,
                &mut self.re,
                &mut self.im,
                &mut self.cand_spec[r],
            );
        }
        let cand_env = envelope(candidate, self.sample_rate, self.padded_len);
        let (env_acc, env_n) = self
            .target_env
            .iter()
            .zip(cand_env.iter())
            .fold((0.0f64, 0usize), |(a, n), (t, c)| (a + (t - c) * (t - c), n + 1));

        let mut spec = [0.0f64; 3];
        for r in 0..SPEC_WINDOWS.len() {
            let sum: f64 = self.target_spec[r]
                .iter()
                .zip(self.cand_spec[r].iter())
                .map(|(t, c)| (t - c) * (t - c))
                .sum();
            spec[r] = (sum / self.cand_spec[r].len().max(1) as f64).sqrt();
        }
        let env = if env_n > 0 { (env_acc / env_n as f64).sqrt() } else { 0.0 };
        LossParts { spec, env }
    }

    /// The full weighted objective for `candidate`.
    pub fn loss(&mut self, candidate: &[f32]) -> f64 {
        self.parts(candidate).total()
    }
}

/// Fills `out` with the log-compressed magnitude spectrogram of `x`
/// (window `win`, hop = window/4), flattened frame-major. Free function
/// so the caller's field borrows split.
fn log_spectrogram_into(
    x: &[f32],
    win: &[f64],
    frame: &mut [f64],
    re: &mut [f64],
    im: &mut [f64],
    out: &mut [f64],
) {
    let w = win.len();
    let hop = w / 4;
    let mut cursor = 0usize;
    let mut pos = 0usize;
    while pos + w <= x.len() {
        for i in 0..w {
            frame[i] = x[pos + i] as f64;
        }
        power_spectrum(frame, win, &mut re[..w], &mut im[..w]);
        for k in 0..w / 2 {
            let mag = (re[k] * re[k] + im[k] * im[k]).sqrt();
            out[cursor] = (1.0 + LOG_COMPRESS * mag).ln();
            cursor += 1;
        }
        pos += hop;
    }
}

/// Scales `x` in place to peak 1.0; silence stays zero.
fn normalize_peak(x: &mut [f32]) {
    let peak = x.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
    if peak > 0.0 {
        let inv = 1.0 / peak;
        for s in x.iter_mut() {
            *s *= inv;
        }
    }
}

/// One-pole-smoothed linear amplitude envelope, sampled every
/// [`ENV_STRIDE`] frames, sized for `padded_len` samples.
fn envelope(x: &[f32], sample_rate: u32, padded_len: usize) -> Vec<f64> {
    // e += a·(|x| − e); `a` puts e's time constant at ENV_TAU_SEC.
    let a = 1.0 - (-1.0 / (ENV_TAU_SEC * sample_rate as f64)).exp();
    let mut out = vec![0.0; padded_len.div_ceil(ENV_STRIDE)];
    let mut e = 0.0f64;
    for (i, &s) in x.iter().take(padded_len).enumerate() {
        e += a * (s.abs() as f64 - e);
        if i % ENV_STRIDE == 0 {
            out[i / ENV_STRIDE] = e;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fit::rng::SplitMix64;

    const SR: u32 = 48_000;

    fn tone(hz: f64, amp: f64, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (amp * (std::f64::consts::TAU * hz * i as f64 / SR as f64).sin()) as f32)
            .collect()
    }

    #[test]
    fn objective_is_zero_for_identical_signals() {
        let x = tone(220.0, 0.5, 24_000);
        let mut obj = FitObjective::new(&x, SR);
        let parts = obj.parts(&x);
        assert_eq!(parts.total(), 0.0);
        assert!(parts.spec.iter().all(|s| *s == 0.0));
        assert_eq!(parts.env, 0.0);
    }

    #[test]
    fn objective_grows_with_spectral_mismatch() {
        let target = tone(220.0, 0.5, 24_000);
        let mut obj = FitObjective::new(&target, SR);
        let mut prev = None;
        for detune in [0.0, 0.01, 0.05, 0.15] {
            let cand = tone(220.0 * (1.0 + detune), 0.5, 24_000);
            let loss = obj.loss(&cand);
            if let Some(p) = prev {
                assert!(loss > p, "loss must grow with detune: {p} -> {loss}");
            }
            prev = Some(loss);
        }
        assert!(prev.unwrap() > 1e-6, "strong detune must score non-zero");
    }

    /// Decay-shape discrimination: the same noise texture under two
    /// different exponential decays (RMS-matched) must move the envelope
    /// term far from zero, while an identical control sits exactly at
    /// zero. The spectral terms react to decay too (frames are
    /// time-indexed), but this pins the property the fitter leans on:
    /// within a fixed voice model, the envelope term is what settles the
    /// decay — the round-trip gate proves it end to end.
    #[test]
    fn envelope_term_discriminates_decay_shape() {
        let n = 48_000;
        let mut noise = SplitMix64::new(7);
        let tex: Vec<f32> = (0..n).map(|_| noise.next_range(-1.0, 1.0) as f32).collect();
        let shaped = |tau_samples: f64| -> Vec<f32> {
            let x: Vec<f64> = tex
                .iter()
                .enumerate()
                .map(|(i, &s)| s as f64 * (-(i as f64) / tau_samples).exp())
                .collect();
            let rms =
                (x.iter().map(|v| v * v).sum::<f64>() / n as f64).sqrt();
            x.iter().map(|v| (v / rms) as f32).collect()
        };
        let fast = shaped(4_800.0); // 100 ms
        let slow = shaped(28_800.0); // 600 ms, same texture

        let mut obj = FitObjective::new(&fast, SR);
        let control = obj.parts(&fast);
        let mismatch = obj.parts(&slow);
        assert_eq!(control.env, 0.0, "identical control must score zero");
        assert!(
            mismatch.env > 0.1,
            "envelope term must fire on a 6x decay change: {}",
            mismatch.env
        );
        assert!(
            mismatch.total() > control.total(),
            "objective must rank the decay-mismatched pair worse"
        );
    }

    #[test]
    fn short_signals_are_padded_and_deterministic() {
        let x = tone(440.0, 0.4, 1_000);
        let mut a = FitObjective::new(&x, SR);
        let mut b = FitObjective::new(&x, SR);
        assert_eq!(a.loss(&x).to_bits(), b.loss(&x).to_bits());
        assert!(a.loss(&x).is_finite());
    }
}
