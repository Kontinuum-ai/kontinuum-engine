//! BS.1770-style loudness measurement (offline, hand-rolled), true-peak
//! estimation, and the loudness-normalization export pass (#28 item 6).
//!
//! Implementation notes / honest deviations from full ITU-R BS.1770-4:
//! - K-weighting is the two-stage RBJ approximation of the spec's analog
//!   prototypes (high shelf ≈ 1681 Hz +4 dB, high-pass ≈ 38 Hz). At 48 kHz
//!   this matches published coefficient tables to well under 0.1 LU; at
//!   other rates the RBJ bilinear recompute is an approximation.
//! - 400 ms mean-square blocks on a 100 ms hop (75% overlap) and 3 s
//!   short-term windows are built from uniform 100 ms energy slots, so
//!   block edges align to slot boundaries (equivalent to the spec grid
//!   anchored at sample 0).
//! - Gating: absolute −70 LUFS, then relative −10 LU below the ungated
//!   mean, per spec. LRA (EBU R128): 10th–95th percentile of short-term
//!   values passing the −20 LU relative gate.

use crate::filters::Biquad;
use crate::oversample::Oversampler4x;

/// Slot length all windows are built from: 100 ms.
const SLOT_MS: f64 = 100.0;
/// K-weighting shelf (spec: +3.99984 dB @ 1681.97 Hz, Q 0.70718).
const SHELF_F0: f64 = 1681.974_450_955_532;
const SHELF_GAIN_DB: f64 = 3.999_843_853_973_347;
const SHELF_Q: f64 = 0.707_175_236_955_420;
/// K-weighting rumble high-pass (spec: 38.135 Hz, Q 0.50033).
const HP_F0: f64 = 38.135_470_876_024_44;
const HP_Q: f64 = 0.500_327_037_323_877;
/// −0.691 + 10·log10(mean square), the spec's calibration offset.
const OFFSET: f64 = -0.691;
const ABSOLUTE_GATE_LUFS: f64 = -70.0;

fn to_db(ms: f64) -> f64 {
    OFFSET + 10.0 * ms.max(1e-24).log10()
}

/// Integrated / short-term-peak / loudness-range triple for a render.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LoudnessMeasurement {
    pub integrated_lufs: f64,
    /// Highest 3 s short-term loudness (ungated), or `NEG_INFINITY` if
    /// the render never rises above the absolute gate.
    pub short_term_peak_lufs: f64,
    pub lra_lu: f64,
}

/// Mean-square energy per uniform 100 ms slot, K-weighted, both channels
/// summed per the spec (G = 1.0 for L/R).
fn kweighted_slot_ms(left: &[f32], right: &[f32], sample_rate: u32) -> Vec<f64> {
    let sr = sample_rate as f64;
    let slot = (sr * SLOT_MS / 1000.0).round() as usize;
    let n = left.len().min(right.len());
    let mut shelves = [Biquad::identity(), Biquad::identity()];
    let mut hps = [Biquad::identity(), Biquad::identity()];
    let shelf = crate::filters::high_shelf_coeffs(sr, SHELF_F0, SHELF_GAIN_DB, SHELF_Q);
    let hp = crate::filters::highpass_coeffs(sr, HP_F0, HP_Q);
    for ch in 0..2 {
        shelves[ch].set_coeffs(shelf);
        hps[ch].set_coeffs(hp);
    }
    let slots = (n + slot - 1) / slot;
    let mut ms = vec![0.0f64; slots];
    for (i, ch_pair) in left.iter().zip(right.iter()).enumerate().take(n) {
        let l = hps[0].tick(shelves[0].tick(*ch_pair.0));
        let r = hps[1].tick(shelves[1].tick(*ch_pair.1));
        ms[i / slot] += (l * l + r * r) as f64;
    }
    for slot_ms in ms.iter_mut() {
        *slot_ms /= slot as f64;
    }
    ms
}

/// Mean square across `count` consecutive slots starting at `start`
/// (slots are uniform, so a plain mean is the window's mean square).
fn window_ms(slots: &[f64], start: usize, count: usize) -> f64 {
    let end = (start + count).min(slots.len());
    if end <= start {
        return 0.0;
    }
    slots[start..end].iter().sum::<f64>() / (end - start) as f64
}

fn percentile(sorted: &[f64], pct: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NEG_INFINITY;
    }
    let idx = ((pct / 100.0) * (sorted.len() - 1) as f64).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// Integrated loudness (LUFS) with the two-stage gate, or
/// `NEG_INFINITY` when nothing passes the absolute gate (silence).
pub fn integrated_lufs(left: &[f32], right: &[f32], sample_rate: u32) -> f64 {
    let slots = kweighted_slot_ms(left, right, sample_rate);
    let block_slots = 4; // 400 ms
    let mut block_loudness: Vec<(f64, f64)> = Vec::new(); // (loudness, ms)
    for start in 0..slots.len().saturating_sub(block_slots - 1) {
        let ms = window_ms(&slots, start, block_slots);
        block_loudness.push((to_db(ms), ms));
    }
    let above_abs: Vec<&(f64, f64)> =
        block_loudness.iter().filter(|(l, _)| *l > ABSOLUTE_GATE_LUFS).collect();
    if above_abs.is_empty() {
        return f64::NEG_INFINITY;
    }
    let rel_ms = above_abs.iter().map(|(_, ms)| *ms).sum::<f64>() / above_abs.len() as f64;
    let relative_gate = to_db(rel_ms) - 10.0;
    let gated: Vec<f64> = above_abs
        .iter()
        .filter(|(l, _)| *l > relative_gate)
        .map(|(_, ms)| *ms)
        .collect();
    if gated.is_empty() {
        return f64::NEG_INFINITY;
    }
    to_db(gated.iter().sum::<f64>() / gated.len() as f64)
}

/// Full measurement: integrated + short-term peak + LRA.
pub fn measure_loudness(left: &[f32], right: &[f32], sample_rate: u32) -> LoudnessMeasurement {
    let slots = kweighted_slot_ms(left, right, sample_rate);
    let block_slots = 4;
    let st_slots = 30; // 3 s short-term

    let mut block_loudness: Vec<(f64, f64)> = Vec::new();
    let mut short_term: Vec<f64> = Vec::new();
    for start in 0..slots.len() {
        if start + block_slots <= slots.len() {
            let ms = window_ms(&slots, start, block_slots);
            block_loudness.push((to_db(ms), ms));
        }
        if start + st_slots <= slots.len() {
            short_term.push(to_db(window_ms(&slots, start, st_slots)));
        }
    }

    let above_abs: Vec<&(f64, f64)> =
        block_loudness.iter().filter(|(l, _)| *l > ABSOLUTE_GATE_LUFS).collect();
    let integrated = if above_abs.is_empty() {
        f64::NEG_INFINITY
    } else {
        let rel_ms = above_abs.iter().map(|(_, ms)| *ms).sum::<f64>() / above_abs.len() as f64;
        let relative_gate = to_db(rel_ms) - 10.0;
        let gated: Vec<f64> = above_abs
            .iter()
            .filter(|(l, _)| *l > relative_gate)
            .map(|(_, ms)| *ms)
            .collect();
        if gated.is_empty() {
            f64::NEG_INFINITY
        } else {
            to_db(gated.iter().sum::<f64>() / gated.len() as f64)
        }
    };

    // LRA: short-term values above the absolute gate and above −20 LU
    // relative to their own (gated) mean; 10th–95th percentile spread.
    let st_abs: Vec<f64> = short_term.iter().copied().filter(|l| *l > ABSOLUTE_GATE_LUFS).collect();
    let lra = if st_abs.len() < 2 {
        0.0
    } else {
        let st_mean = to_db(
            st_abs.iter().map(|l| 10.0f64.powf((l - OFFSET) / 10.0)).sum::<f64>() / st_abs.len() as f64,
        );
        let gate = st_mean - 20.0;
        let mut kept: Vec<f64> = st_abs.into_iter().filter(|l| *l > gate).collect();
        if kept.len() < 2 {
            0.0
        } else {
            kept.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            percentile(&kept, 95.0) - percentile(&kept, 10.0)
        }
    };

    LoudnessMeasurement {
        integrated_lufs: integrated,
        short_term_peak_lufs: short_term.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        lra_lu: lra,
    }
}

/// Peak of the 4× oversampled signal, in dBFS — the crate's true-peak
/// estimate (see `oversample` for the filter's honest quality).
pub fn true_peak_dbfs(left: &[f32], right: &[f32]) -> f64 {
    let mut os_l = Oversampler4x::new();
    let mut os_r = Oversampler4x::new();
    let mut sub_l = [0.0f32; 4];
    let mut sub_r = [0.0f32; 4];
    let mut peak = 0.0f64;
    for (l, r) in left.iter().zip(right.iter()) {
        os_l.up(*l, &mut sub_l);
        os_r.up(*r, &mut sub_r);
        for k in 0..4 {
            peak = peak.max((sub_l[k] as f64).abs()).max((sub_r[k] as f64).abs());
        }
    }
    20.0 * peak.max(1e-12).log10()
}

/// Result of [`normalize_to_target`].
#[derive(Clone, Debug, PartialEq)]
pub struct NormalizedRender {
    pub left: Vec<f32>,
    pub right: Vec<f32>,
    /// Gain applied from the loudness measurement (dB; 0 for silence).
    pub gain_db: f64,
    /// Extra peak-scaling applied to respect the ceiling (dB, ≤ 0).
    pub ceiling_trim_db: f64,
    pub integrated_lufs: f64,
}

/// Loudness-normalize to `target_lufs`, then respect `ceiling_dbtp` by
/// plain peak scaling (an export pass, not the real-time limiter — a
/// heavily limited master that would need the limiter again indicates a
/// bad target, so scaling + a louder source is the honest fallback).
/// Silence passes through untouched (measuring silence has no solution).
pub fn normalize_to_target(
    left: &[f32],
    right: &[f32],
    sample_rate: u32,
    target_lufs: f64,
    ceiling_dbtp: f64,
) -> NormalizedRender {
    let measured = integrated_lufs(left, right, sample_rate);
    let gain_db = if measured == f64::NEG_INFINITY {
        0.0
    } else {
        (target_lufs - measured).min(24.0).max(-24.0)
    };
    let g = 10.0f64.powf(gain_db / 20.0);
    let mut out_l: Vec<f32> = left.iter().map(|s| (s * g as f32)).collect();
    let mut out_r: Vec<f32> = right.iter().map(|s| (s * g as f32)).collect();
    let tp = true_peak_dbfs(&out_l, &out_r);
    let mut trim = 0.0f64;
    if tp > ceiling_dbtp {
        trim = ceiling_dbtp - tp;
        let t = 10.0f64.powf(trim / 20.0) as f32;
        for s in out_l.iter_mut().chain(out_r.iter_mut()) {
            *s *= t;
        }
    }
    let final_lufs = integrated_lufs(&out_l, &out_r, sample_rate);
    NormalizedRender {
        left: out_l,
        right: out_r,
        gain_db,
        ceiling_trim_db: trim,
        integrated_lufs: final_lufs,
    }
}
