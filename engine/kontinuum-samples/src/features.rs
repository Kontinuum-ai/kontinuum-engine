//! In-crate feature extraction (#19 build pipeline): the hand-engineered
//! catalog features, computed deterministically from normalized PCM.
//!
//! Approximations, honestly: the spectral centroid is a Goertzel-band
//! estimate — band powers at 24 log-spaced centers from 20 Hz to Nyquist
//! (capped at 20 kHz), whole-signal so leakage is included — with the band
//! frequency floored at 20 Hz so log-scale consumers stay valid (and the
//! catalog's `centroid > 0` bound can never trip). Flatness is the
//! geometric/arithmetic power ratio over the same bands. Transient
//! sharpness is the largest positive step of a 5 ms RMS envelope,
//! normalized by the envelope peak. Loudness is an un-weighted RMS proxy
//! for LUFS (no K-weighting; the −0.691 full-scale alignment is applied;
//! digital silence floors at −120 since true LUFS is −inf). `pitch_hz` is
//! 0.0 — pitch estimation is filled in later by the #20 build pipeline.
//!
//! Every output lands inside the bounds `crate::catalog::parse_catalog`
//! enforces — finite, centroid > 0, flatness/sharpness in 0..=1 — including
//! for digital silence.

use crate::catalog::EngineeredFeatures;

const CENTROID_FLOOR_HZ: f32 = 20.0;
const BAND_COUNT: usize = 24;
/// Loudness floor for silent material (true LUFS is undefined at −inf).
const LUFS_SILENCE: f32 = -120.0;

/// Extracts the catalog features for one (normalized) sample.
pub fn analyze_features(pcm: &[f32], sample_rate: u32) -> EngineeredFeatures {
    let nyquist = (sample_rate as f32 / 2.0).min(20_000.0);
    let bands: Vec<(f32, f32)> = (0..BAND_COUNT)
        .map(|i| {
            let f = CENTROID_FLOOR_HZ
                * (nyquist / CENTROID_FLOOR_HZ).powf(i as f32 / (BAND_COUNT - 1) as f32);
            (f, goertzel_power(pcm, f, sample_rate))
        })
        .collect();

    let total: f32 = bands.iter().map(|(_, p)| *p).sum();
    let centroid = if total > 0.0 {
        bands
            .iter()
            .map(|(f, p)| f.max(CENTROID_FLOOR_HZ) * p)
            .sum::<f32>()
            / total
    } else {
        CENTROID_FLOOR_HZ // silence: the floor, never NaN
    };

    let ms = mean_square(pcm);
    EngineeredFeatures {
        duration_s: pcm.len() as f32 / sample_rate as f32,
        spectral_centroid_hz: centroid,
        flatness: flatness_of(&bands, total),
        pitch_hz: 0.0, // filled by the #20 build pipeline's pitch estimator
        transient_sharpness: transient_sharpness(pcm, sample_rate),
        lufs: if ms > 0.0 { -0.691 + 10.0 * ms.log10() } else { LUFS_SILENCE },
    }
}

/// Goertzel power of `pcm` at `freq` (whole-signal bin; tiny negative
/// results from float error clamp to 0).
fn goertzel_power(pcm: &[f32], freq: f32, sample_rate: u32) -> f32 {
    let w = std::f32::consts::TAU * freq / sample_rate as f32;
    let coeff = 2.0 * w.cos();
    let (mut s1, mut s2) = (0.0f32, 0.0f32);
    for &x in pcm {
        let s = x + coeff * s1 - s2;
        s2 = s1;
        s1 = s;
    }
    (s1 * s1 + s2 * s2 - coeff * s1 * s2).max(0.0)
}

/// Geometric / arithmetic mean of band powers: near 0 for pure tones,
/// → 1 for noise; 0.0 for silence or any empty band (ln 0 → product 0).
fn flatness_of(bands: &[(f32, f32)], total: f32) -> f32 {
    if total <= 0.0 || bands.is_empty() {
        return 0.0;
    }
    let log_sum: f32 = bands.iter().map(|(_, p)| p.ln()).sum();
    let arith = total / bands.len() as f32;
    if !log_sum.is_finite() || arith <= 0.0 {
        return 0.0;
    }
    ((log_sum / bands.len() as f32).exp() / arith).clamp(0.0, 1.0)
}

/// Largest positive step of a 5 ms RMS envelope, normalized by the envelope
/// peak: sharp onsets → near 1, slow swells → near 0, silence → 0.
fn transient_sharpness(pcm: &[f32], sample_rate: u32) -> f32 {
    let win = (sample_rate as usize / 200).max(1); // 5 ms
    let env: Vec<f32> = pcm.chunks(win).map(|c| mean_square(c).sqrt()).collect();
    if env.len() < 2 {
        return 0.0;
    }
    let peak = env.iter().fold(0.0f32, |m, e| m.max(*e));
    if peak <= 0.0 {
        return 0.0;
    }
    let rise = env.windows(2).map(|w| (w[1] - w[0]).max(0.0)).fold(0.0f32, f32::max);
    (rise / peak).clamp(0.0, 1.0)
}

fn mean_square(pcm: &[f32]) -> f32 {
    if pcm.is_empty() {
        return 0.0;
    }
    pcm.iter().map(|s| s * s).sum::<f32>() / pcm.len() as f32
}
