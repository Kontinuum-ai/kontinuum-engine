//! Per-bar feature curves for the corpus pipeline: energy (RMS),
//! density (onsets per bar), brightness (spectral centroid). Energy and
//! brightness are normalized 0..1 by the track max; density stays a raw
//! per-bar onset count (the boundary classifier needs absolute fill
//! spikes).

use crate::corpus::onsets::Onset;
use crate::corpus::tempo::BeatGrid;
use crate::fft::{hanning, next_pow2, power_spectrum};

const CENTROID_WINDOW: usize = 8192;
const HIGH_LO_HZ: f64 = 2_000.0;
const HIGH_HI_HZ: f64 = 8_000.0;
const TOP_LO_HZ: f64 = 300.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BarFeatures {
    /// Bar RMS relative to the loudest bar, 0..1.
    pub energy: f64,
    /// Raw onset count in the bar.
    pub density: f64,
    /// High-band energy share (2k–8 kHz of total bar power), 0..1. A
    /// share, not a centroid: hats + kick click pin a centroid high in
    /// every section, while the share actually tracks pads opening and
    /// risers sweeping.
    pub brightness: f64,
    /// Summed onset strength in the bar (fill spikes against it).
    pub flux: f64,
}

/// One feature triple per whole 4-beat bar on the grid. A near-silent bar
/// carries the previous brightness forward — silence has no color, and a
/// fake centroid collapse there would masquerade as a boundary.
pub fn per_bar(mono: &[f32], sr: u32, grid: &BeatGrid, onsets: &[Onset]) -> Vec<BarFeatures> {
    let beat = grid.beat_sec();
    let bar_sec = 4.0 * beat;
    let bars = grid.total_bars(mono.len() as f64 / f64::from(sr)) as usize;
    let mut out = Vec::with_capacity(bars);

    let mut raw_energy = Vec::with_capacity(bars);
    let mut raw_share = Vec::with_capacity(bars);
    let mut density = Vec::with_capacity(bars);
    let mut flux_sum = Vec::with_capacity(bars);
    for b in 0..bars {
        let bar_start_sec = grid.first_beat_sec + b as f64 * bar_sec;
        let start = (bar_start_sec * f64::from(sr)) as usize;
        let end = ((bar_start_sec + bar_sec) * f64::from(sr)) as usize;
        let span = &mono[start.min(mono.len())..end.min(mono.len())];
        let rms = (span.iter().map(|x| f64::from(*x) * f64::from(*x)).sum::<f64>()
            / span.len().max(1) as f64)
            .sqrt();
        raw_energy.push(rms);
        raw_share.push(high_share_at(mono, sr, start, span, rms));
        let (count, flux): (u32, f64) = onsets
            .iter()
            .filter(|o| {
                let t = o.time_sec - grid.first_beat_sec;
                t >= b as f64 * bar_sec && t < (b + 1) as f64 * bar_sec
            })
            .fold((0, 0.0), |(n, s), o| (n + 1, s + o.strength));
        density.push(f64::from(count));
        flux_sum.push(flux);
    }

    let max_energy = raw_energy.iter().cloned().fold(1e-12, f64::max);
    fill_gaps(&mut raw_share);
    for b in 0..bars {
        out.push(BarFeatures {
            energy: raw_energy[b] / max_energy,
            density: density[b],
            brightness: raw_share[b].clamp(0.0, 1.0),
            flux: flux_sum[b],
        });
    }
    out
}

/// Replaces NaN holes (silent bars) by carrying the previous brightness
/// forward; leading holes take the track median, and an all-silent track
/// reads 0.
fn fill_gaps(values: &mut [f64]) {
    let mut known: Vec<f64> = values.iter().copied().filter(|v| !v.is_nan()).collect();
    known.sort_by(f64::total_cmp);
    let median = known.get(known.len() / 2).copied().unwrap_or(0.0);
    let mut last = median;
    for v in values.iter_mut() {
        if v.is_nan() {
            *v = last;
        } else {
            last = *v;
        }
    }
}

/// Brightness of the top mix: share of above-300 Hz power that lives in
/// 2k–8 kHz (sub/bass excluded from the denominator — a 909 kick would
/// otherwise make every master read pitch-black). One power spectrum at
/// the bar's center; NaN for near-silent bars.
fn high_share_at(mono: &[f32], sr: u32, start: usize, span: &[f32], rms: f64) -> f64 {
    if rms < 1e-4 {
        return f64::NAN;
    }
    let padded = next_pow2(CENTROID_WINDOW);
    let win = hanning(CENTROID_WINDOW);
    let bins = padded / 2;
    let mut scratch = [0.0f64; CENTROID_WINDOW];
    let fill = CENTROID_WINDOW.min(mono.len().saturating_sub(start)).min(span.len() * 4);
    for (i, slot) in scratch.iter_mut().enumerate().take(fill) {
        *slot = f64::from(mono[start + i]);
    }
    let mut re = vec![0.0f64; padded];
    let mut im = vec![0.0f64; padded];
    power_spectrum(&scratch, &win, &mut re, &mut im);
    let mut high = 0.0f64;
    let mut top = 0.0f64;
    for k in 0..bins {
        let f = k as f64 * f64::from(sr) / padded as f64;
        let p = re[k] * re[k] + im[k] * im[k];
        if f >= TOP_LO_HZ {
            top += p;
        }
        if (HIGH_LO_HZ..HIGH_HI_HZ).contains(&f) {
            high += p;
        }
    }
    if top > 0.0 {
        high / top
    } else {
        f64::NAN
    }
}
