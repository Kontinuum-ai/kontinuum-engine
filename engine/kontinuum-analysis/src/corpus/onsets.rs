//! Onset detection for the corpus pipeline: frame-RMS positive flux with
//! adaptive peak picking. One pass feeds three consumers — bar density,
//! boundary fill detection, and groove microtiming.

use crate::filters::{highpass_coeffs, lowpass_coeffs, Biquad};

/// Analysis hop in samples (~5.8 ms at the fixture rate).
pub const HOP: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Onset {
    pub time_sec: f64,
    /// Relative peak strength, 0..1 (normalized by the track's strongest
    /// onset).
    pub strength: f64,
}

/// Frame RMS envelope at [`HOP`] resolution.
fn frame_rms(mono: &[f32], sr: u32) -> Vec<f64> {
    let mut rms = Vec::with_capacity(mono.len() / HOP + 1);
    for chunk in mono.chunks(HOP) {
        let s: f64 = chunk.iter().map(|x| f64::from(*x) * f64::from(*x)).sum();
        rms.push((s / chunk.len().max(1) as f64).sqrt());
    }
    let _ = sr;
    rms
}

/// Positive-difference onset envelope (per frame, ≥ 0) of the raw signal.
pub fn flux_envelope(mono: &[f32], sr: u32) -> Vec<f64> {
    band_flux_envelope(mono, sr, None)
}

/// Positive-difference onset envelope (per frame, ≥ 0), optionally of a
/// band-passed copy of the signal (the percussive-band method: swing and
/// microtiming live on the hats, where the kick cannot mask them).
pub fn band_flux_envelope(mono: &[f32], sr: u32, band: Option<(f64, f64)>) -> Vec<f64> {
    let owned: Vec<f32>;
    let src: &[f32] = match band {
        None => mono,
        Some((lo, hi)) => {
            let srf = f64::from(sr);
            let mut stages = [
                Biquad::identity(),
                Biquad::identity(),
                Biquad::identity(),
                Biquad::identity(),
            ];
            stages[0].set_coeffs(highpass_coeffs(srf, lo, std::f64::consts::FRAC_1_SQRT_2));
            stages[1].set_coeffs(highpass_coeffs(srf, lo, std::f64::consts::FRAC_1_SQRT_2));
            stages[2].set_coeffs(lowpass_coeffs(srf, hi, std::f64::consts::FRAC_1_SQRT_2));
            stages[3].set_coeffs(lowpass_coeffs(srf, hi, std::f64::consts::FRAC_1_SQRT_2));
            owned = mono
                .iter()
                .copied()
                .map(|x| {
                    let mut s = x;
                    for stage in stages.iter_mut() {
                        s = stage.tick(s);
                    }
                    s
                })
                .collect();
            &owned
        }
    };
    let rms = frame_rms(src, sr);
    let mut env = Vec::with_capacity(rms.len());
    let mut prev = 0.0f64;
    for &r in &rms {
        env.push((r - prev).max(0.0));
        prev = r;
    }
    // Light smoothing so one noisy frame cannot split into two onsets.
    env.windows(3)
        .map(|w| 0.25 * w[0] + 0.5 * w[1] + 0.25 * w[2])
        .collect()
}

/// Peak-picks onsets with a locally adaptive threshold: a candidate (local
/// maximum over ±2 frames, minimum 30 ms spacing) counts when it exceeds
/// twice the mean flux of its ±0.5 s neighborhood — so a breakdown roll
/// (loud for its quiet section, moderate for the track) still registers.
/// A weak global floor discards silence noise.
pub fn pick_from(env: &[f64], frame_sec: f64) -> Vec<Onset> {
    let min_gap_frames = (0.03 / frame_sec).ceil() as usize;
    let local_window = (0.5 / frame_sec) as usize;
    let max = env.iter().cloned().fold(0.0f64, f64::max);
    let mut out = Vec::new();
    let mut last_pick = usize::MAX;
    for (i, &e) in env.iter().enumerate() {
        if e < 0.05 * max {
            continue;
        }
        let lo = i.saturating_sub(2);
        let hi = (i + 3).min(env.len());
        if env[lo..hi].iter().any(|&x| x > e) {
            continue;
        }
        if last_pick != usize::MAX && i - last_pick < min_gap_frames {
            continue;
        }
        let wlo = i.saturating_sub(local_window);
        let whi = (i + local_window).min(env.len());
        let local_mean = (env[wlo..whi].iter().sum::<f64>()) / (whi - wlo) as f64;
        if e < 2.0 * local_mean {
            continue;
        }
        out.push(Onset { time_sec: i as f64 * frame_sec, strength: e / max });
        last_pick = i;
    }
    out
}

/// Broadband onsets (density, fill detection, boundaries).
pub fn pick_onsets(mono: &[f32], sr: u32) -> Vec<Onset> {
    pick_from(&flux_envelope(mono, sr), HOP as f64 / f64::from(sr))
}

/// Percussive-band onsets (4–10 kHz — the hats): the microtiming/velocity
/// source for groove stats.
pub fn pick_percussive_onsets(mono: &[f32], sr: u32) -> Vec<Onset> {
    pick_from(
        &band_flux_envelope(mono, sr, Some((4_000.0, 10_000.0))),
        HOP as f64 / f64::from(sr),
    )
}
