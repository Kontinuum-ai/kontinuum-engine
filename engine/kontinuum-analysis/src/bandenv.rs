//! Beat-phase-averaged band envelopes (issue #76's acceptance metric):
//! band-limit a render, fold its energy into 32 beat-phase bins, and
//! report the envelope's range in dB — the number that tells a pumping
//! mix from a drone. The methodology mirrors the hand measurement that
//! motivated #76 (32 bins across one beat, range in dB).

use crate::filters::{highpass_coeffs, lowpass_coeffs, Biquad};

/// Bins per beat in the phase average.
pub const PHASE_BINS: usize = 32;
/// Sub band (Hz) — kick/bass collision territory.
pub const SUB_BAND: (f64, f64) = (30.0, 100.0);
/// Mid band (Hz) — where sustained harmonic content masks the beat.
pub const MID_BAND: (f64, f64) = (400.0, 2_000.0);

/// One band's beat-phase profile: energy-average RMS per phase bin.
#[derive(Clone, Copy, Debug)]
pub struct BandEnvelope {
    /// `max − min` of the 32-bin profile, dB.
    pub range_db: f64,
    /// The profile itself, dB relative to the loudest bin.
    pub profile_db: [f64; PHASE_BINS],
}

/// Cascades `2 × highpass + 2 × lowpass` biquads (24 dB/oct per edge,
/// Butterworth Q) — the same shape as the reference measurement.
fn bandpass(mono: &[f32], sample_rate: u32, lo_hz: f64, hi_hz: f64) -> Vec<f32> {
    let sr = f64::from(sample_rate);
    let mut stages = [
        Biquad::identity(),
        Biquad::identity(),
        Biquad::identity(),
        Biquad::identity(),
    ];
    stages[0].set_coeffs(highpass_coeffs(sr, lo_hz, std::f64::consts::FRAC_1_SQRT_2));
    stages[1].set_coeffs(highpass_coeffs(sr, lo_hz, std::f64::consts::FRAC_1_SQRT_2));
    stages[2].set_coeffs(lowpass_coeffs(sr, hi_hz, std::f64::consts::FRAC_1_SQRT_2));
    stages[3].set_coeffs(lowpass_coeffs(sr, hi_hz, std::f64::consts::FRAC_1_SQRT_2));
    mono.iter()
        .copied()
        .map(|x| {
            let mut s = x;
            for stage in stages.iter_mut() {
                s = stage.tick(s);
            }
            s
        })
        .collect()
}

/// Beat-phase-averaged envelope of `mono` in `lo_hz..hi_hz`. `skip_beats`
/// drops the lead-in (render priming ramps) before folding; the fold runs
/// over whole beats only. `bpm` must match the render's tempo lane.
pub fn beat_band_envelope(
    mono: &[f32],
    sample_rate: u32,
    bpm: f64,
    lo_hz: f64,
    hi_hz: f64,
    skip_beats: usize,
) -> BandEnvelope {
    let filtered = bandpass(mono, sample_rate, lo_hz, hi_hz);
    let beat_frames = (60.0 / bpm * f64::from(sample_rate)) as usize;
    let start = skip_beats * beat_frames;
    let beats = (filtered.len().saturating_sub(start)) / beat_frames;
    let mut energy = [0.0f64; PHASE_BINS];
    let mut counts = [0u64; PHASE_BINS];
    for beat in 0..beats {
        let base = start + beat * beat_frames;
        for (frame, &x) in filtered[base..base + beat_frames].iter().enumerate() {
            let bin = frame * PHASE_BINS / beat_frames;
            energy[bin] += f64::from(x) * f64::from(x);
            counts[bin] += 1;
        }
    }
    let mut profile_db = [0.0f64; PHASE_BINS];
    let rms: Vec<f64> =
        energy.iter().zip(counts).map(|(&e, c)| (e / c.max(1) as f64).sqrt()).collect();
    let max = rms.iter().cloned().fold(0.0f64, f64::max);
    for (slot, r) in profile_db.iter_mut().zip(&rms) {
        *slot = 20.0 * (r / max.max(1e-12)).log10();
    }
    let min = profile_db.iter().cloned().fold(f64::MAX, f64::min);
    let range_db = max_db(&profile_db) - min;
    BandEnvelope { range_db, profile_db }
}

fn max_db(profile: &[f64; PHASE_BINS]) -> f64 {
    profile.iter().cloned().fold(f64::MIN, f64::max)
}

/// Sub and mid envelopes for one render, computed once.
pub fn sub_and_mid_envelopes(
    mono: &[f32],
    sample_rate: u32,
    bpm: f64,
    skip_beats: usize,
) -> (BandEnvelope, BandEnvelope) {
    (
        beat_band_envelope(mono, sample_rate, bpm, SUB_BAND.0, SUB_BAND.1, skip_beats),
        beat_band_envelope(mono, sample_rate, bpm, MID_BAND.0, MID_BAND.1, skip_beats),
    )
}

/// (sub, mid) phase-averaged range of one window, in dB.
pub type WindowRanges = (f64, f64);

/// Slides a `window_beats`-long window over the render in `hop_beats`
/// steps and reports each window's (sub, mid) beat-phase-averaged range.
/// An arrangement breathes — breakdowns strip the pump bare — so the
/// acceptance question is whether the record CONTAINS a groove window
/// where both bands swing past the bar, not whether every section does.
pub fn pump_window_ranges(
    mono: &[f32],
    sample_rate: u32,
    bpm: f64,
    window_beats: usize,
    hop_beats: usize,
) -> Vec<WindowRanges> {
    let beat_frames = (60.0 / bpm * f64::from(sample_rate)) as usize;
    let window_frames = window_beats * beat_frames;
    if mono.len() < window_frames + beat_frames {
        return Vec::new();
    }
    let sub = bandpass(mono, sample_rate, SUB_BAND.0, SUB_BAND.1);
    let mid = bandpass(mono, sample_rate, MID_BAND.0, MID_BAND.1);
    let hop_frames = hop_beats * beat_frames;
    let mut out = Vec::new();
    let mut start = 0;
    while start + window_frames <= mono.len() {
        let s = window_range(&sub[start..start + window_frames], beat_frames);
        let m = window_range(&mid[start..start + window_frames], beat_frames);
        out.push((s, m));
        start += hop_frames;
    }
    out
}

/// Phase-averaged range (dB) of one already-band-limited window.
fn window_range(band: &[f32], beat_frames: usize) -> f64 {
    let beats = band.len() / beat_frames;
    let mut energy = [0.0f64; PHASE_BINS];
    let mut counts = [0u64; PHASE_BINS];
    for beat in 0..beats {
        let base = beat * beat_frames;
        for (frame, &x) in band[base..base + beat_frames].iter().enumerate() {
            let bin = frame * PHASE_BINS / beat_frames;
            energy[bin] += f64::from(x) * f64::from(x);
            counts[bin] += 1;
        }
    }
    let rms: Vec<f64> =
        energy.iter().zip(counts).map(|(&e, c)| (e / c.max(1) as f64).sqrt()).collect();
    let max = rms.iter().cloned().fold(0.0f64, f64::max);
    let min = rms.iter().cloned().fold(f64::MAX, f64::min);
    20.0 * (max / min.max(1e-12)).log10()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A steady sine has a flat envelope; gating it half off per beat makes
    /// the range approach the full depth.
    #[test]
    fn gated_tone_measures_a_range_and_steady_tone_does_not() {
        let sr = 48_000u32;
        let bpm = 120.0;
        let beat = (60.0 / bpm * sr as f64) as usize;
        let frames = beat * 16;
        let steady: Vec<f32> = (0..frames)
            .map(|i| 0.5 * (std::f32::consts::TAU * 700.0 * i as f32 / sr as f32).sin())
            .collect();
        let mut gated = steady.clone();
        for beat_index in 0..16 {
            let base = beat_index * beat;
            for slot in gated[base..base + beat / 2].iter_mut() {
                *slot = 0.0;
            }
        }
        let steady_env = beat_band_envelope(&steady, sr, bpm, 400.0, 2_000.0, 2);
        let gated_env = beat_band_envelope(&gated, sr, bpm, 400.0, 2_000.0, 2);
        assert!(
            steady_env.range_db < 1.0,
            "steady tone moved: {}",
            steady_env.range_db
        );
        assert!(
            gated_env.range_db > 6.0,
            "half-gated tone too flat: {}",
            gated_env.range_db
        );
    }
}
