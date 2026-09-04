//! Tempo and beat-grid detection for the corpus pipeline: autocorrelation
//! of the kick-band onset envelope (the #5 percussive-band method's
//! tempo half), then a phase fold to place beat zero on the strongest
//! kick. Documented limits: strict 4/4 dance material, 60–200 BPM — the
//! corpus manifest's declared range.

use crate::corpus::onsets::HOP;
use crate::corpus::AnalysisError;
use crate::filters::{highpass_coeffs, lowpass_coeffs, Biquad};

/// Detected tempo and the grid origin (beat zero) in seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BeatGrid {
    pub bpm: f64,
    pub first_beat_sec: f64,
}

impl BeatGrid {
    pub fn beat_sec(&self) -> f64 {
        60.0 / self.bpm
    }

    /// Number of whole 4-beat bars that fit after the grid origin.
    pub fn total_bars(&self, track_sec: f64) -> u32 {
        ((track_sec - self.first_beat_sec) / (4.0 * self.beat_sec())).floor().max(0.0) as u32
    }
}

/// Kick band: where the 4/4 pulse lives.
const KICK_BAND: (f64, f64) = (40.0, 140.0);

fn kick_band_flux(mono: &[f32], sr: u32) -> Vec<f64> {
    let srf = f64::from(sr);
    let mut stages = [
        Biquad::identity(),
        Biquad::identity(),
        Biquad::identity(),
        Biquad::identity(),
    ];
    stages[0].set_coeffs(highpass_coeffs(srf, KICK_BAND.0, std::f64::consts::FRAC_1_SQRT_2));
    stages[1].set_coeffs(highpass_coeffs(srf, KICK_BAND.0, std::f64::consts::FRAC_1_SQRT_2));
    stages[2].set_coeffs(lowpass_coeffs(srf, KICK_BAND.1, std::f64::consts::FRAC_1_SQRT_2));
    stages[3].set_coeffs(lowpass_coeffs(srf, KICK_BAND.1, std::f64::consts::FRAC_1_SQRT_2));
    // Per-HOP mean rectified energy of the bandpassed signal.
    let mut env: Vec<f64> = mono
        .chunks(HOP)
        .map(|chunk| {
            let s: f64 = chunk
                .iter()
                .copied()
                .map(|x| {
                    let mut s = x;
                    for stage in stages.iter_mut() {
                        s = stage.tick(s);
                    }
                    f64::from(s.abs())
                })
                .sum();
            s / chunk.len().max(1) as f64
        })
        .collect();
    // 3-frame smoothing, then positive diff.
    let smoothed: Vec<f64> = env
        .windows(3)
        .map(|w| 0.25 * w[0] + 0.5 * w[1] + 0.25 * w[2])
        .chain(std::iter::once(0.0))
        .collect();
    env = smoothed;
    let mut flux = Vec::with_capacity(env.len());
    let mut prev = 0.0;
    for &e in &env {
        flux.push((e - prev).max(0.0));
        prev = e;
    }
    flux
}

/// Detects the tempo/beat grid around the manifest's declared BPM (the
/// corpus metadata anchors the search to hint×0.7..hint×1.4 — without it,
/// bass-line periodicity drags dance material to half/double tempo).
/// Errors when no confident beat-period peak exists (near-silent or
/// arrhythmic input).
pub fn detect(mono: &[f32], sr: u32, bpm_hint: f64) -> Result<BeatGrid, AnalysisError> {
    let frame_sec = HOP as f64 / f64::from(sr);
    let env = kick_band_flux(mono, sr);
    let energy: f64 = env.iter().sum();
    if energy <= 0.0 {
        return Err(AnalysisError::TempoFailed);
    }
    let min_lag = ((60.0 / (bpm_hint * 1.4)) / frame_sec).floor() as usize;
    let max_lag = ((60.0 / (bpm_hint * 0.7)) / frame_sec).ceil() as usize;
    if max_lag + 2 >= env.len() {
        return Err(AnalysisError::TooShort { samples: mono.len() });
    }
    let mut best = (min_lag, -1.0f64);
    for lag in min_lag..=max_lag.min(env.len() - 2) {
        let count = env.len() - lag;
        let r: f64 = env[..count].iter().zip(&env[lag..]).map(|(a, b)| a * b).sum();
        let r = r / count as f64;
        if r > best.1 {
            best = (lag, r);
        }
    }
    // Parabolic refinement around the winning lag.
    let (lag, _) = best;
    let y = |l: usize| -> f64 {
        let count = env.len() - l;
        env[..count].iter().zip(&env[l..]).map(|(a, b)| a * b).sum::<f64>() / count as f64
    };
    let (ym1, y0, yp1) = (y(lag - 1), y(lag), y(lag + 1));
    let denom = ym1 - 2.0 * y0 + yp1;
    let shift = if denom.abs() > 1e-12 { (0.5 * (ym1 - yp1) / denom).clamp(-0.5, 0.5) } else { 0.0 };
    let period_frames = lag as f64 + shift;
    let bpm = 60.0 / (period_frames * frame_sec);

    // Phase fold: strongest flux phase within one beat is the kick.
    let period = period_frames;
    let mut phase_acc = vec![0.0f64; period.ceil() as usize];
    let mut phase_n = vec![0u32; phase_acc.len()];
    for (i, &e) in env.iter().enumerate() {
        let slot = (i as f64 % period) as usize;
        phase_acc[slot] += e;
        phase_n[slot] += 1;
    }
    let best_phase = (0..phase_acc.len())
        .max_by(|&a, &b| {
            let va = phase_acc[a] / phase_n[a].max(1) as f64;
            let vb = phase_acc[b] / phase_n[b].max(1) as f64;
            va.total_cmp(&vb)
        })
        .unwrap_or(0);
    let coarse = BeatGrid { bpm, first_beat_sec: best_phase as f64 * frame_sec };
    Ok(refine(&env, frame_sec, coarse))
}

/// Grid-fit refinement: the coarse autocorrelation period quantizes to the
/// envelope's frame grid (~5.8 ms), which drifts bars away by a track's
/// tail. Search small period/phase deviations for the grid that puts the
/// most onset energy ON the lines (Gaussian kernel, σ = 15 ms).
fn refine(env: &[f64], frame_sec: f64, coarse: BeatGrid) -> BeatGrid {
    let max = env.iter().cloned().fold(0.0f64, f64::max);
    let clicks: Vec<(f64, f64)> = env
        .iter()
        .enumerate()
        .filter(|(i, &e)| {
            e > 0.1 * max
                && i > &0
                && *i + 1 < env.len()
                && e >= env[i - 1]
                && e > env[*i + 1]
        })
        .map(|(i, &e)| (i as f64 * frame_sec, e))
        .collect();
    if clicks.is_empty() {
        return coarse;
    }
    let sigma = 0.015_f64;
    let p0 = coarse.beat_sec();
    let mut best = (coarse, -1.0f64);
    for dp in -10..=10i32 {
        let p = p0 * (1.0 + f64::from(dp) * 0.001);
        for dphi in 0..20i32 {
            let phi = coarse.first_beat_sec + f64::from(dphi) * p / 20.0 - p / 2.0;
            let mut score = 0.0f64;
            for &(t, w) in &clicks {
                let r = (t - phi).rem_euclid(p);
                let dist = r.min(p - r);
                score += w * f64::exp(-dist * dist / (2.0 * sigma * sigma));
            }
            if score > best.1 {
                best = (BeatGrid { bpm: 60.0 / p, first_beat_sec: phi.rem_euclid(p) }, score);
            }
        }
    }
    best.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_fails_loudly() {
        let quiet = vec![0.0f32; 22_050 * 5];
        assert!(matches!(detect(&quiet, 22_050, 124.0), Err(AnalysisError::TempoFailed)));
    }
}
