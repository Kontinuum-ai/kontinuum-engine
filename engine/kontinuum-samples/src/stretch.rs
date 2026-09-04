//! Time-stretch (issue #19) behind [`StretchMode`]. Pure Rust, no external
//! or C dependencies.
//!
//! # Modes and their honest quality/CPU trade-offs
//!
//! | Mode | Pitch | Quality | CPU (48 kHz, per voice) |
//! |---|---|---|---|
//! | [`StretchMode::RepitchOnly`] | shifts by the factor | clean (linear-interp resample) | ~1 MAC/sample — negligible |
//! | [`StretchMode::Wsola`] | preserved | good for 0.25×–4× on percussive loops; mild transient smearing on extreme factors, no phase-coherent multichannel support | ~512 MAC/sample (frame search) + OLA — budget a few concurrent loop voices, ~100× repitch cost |
//!
//! WSOLA (waveform-similarity overlap-add): 1024-sample Hann frames,
//! 256-sample synthesis hop, ±256-sample (±5.3 ms @ 48 kHz) correlation
//! search against the previous frame's natural continuation. The search is
//! a pure integer scan with strict-greater tie-breaking, so output is a
//! bit-reproducible function of the input.
//!
//! `factor` is the tempo ratio: 2.0 renders the material twice as fast at
//! half the length. Bounded by [`bounds::STRETCH_FACTOR`].

use serde::{Deserialize, Serialize};

use crate::schema::{bounds, check, RecipeError};

/// Tempo-conformance strategy for sample loops.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StretchMode {
    /// Resample by the tempo factor: CPU-free fallback, pitch follows tempo.
    #[default]
    RepitchOnly,
    /// WSOLA time-stretch: pitch preserved, moderate CPU.
    Wsola,
}

/// WSOLA synthesis frame length.
const FRAME: usize = 1024;
/// WSOLA synthesis (output) hop in frames.
const HOP: usize = 256;
/// Correlation search half-window in frames (±5.3 ms @ 48 kHz).
const TOLERANCE: usize = 256;

/// Stretch (or squeeze) `input` by the tempo `factor` in the given mode.
/// Deterministic: same inputs → bit-identical output.
pub fn stretch(
    input: &[f32],
    sample_rate: u32,
    mode: StretchMode,
    factor: f32,
) -> Result<Vec<f32>, RecipeError> {
    check("stretch factor", factor, bounds::STRETCH_FACTOR)?;
    // ±ms search window is rate-independent: scale the 48 kHz geometry.
    let tolerance = ((TOLERANCE as f32) * sample_rate as f32 / 48_000.0).round() as usize;
    Ok(match mode {
        StretchMode::RepitchOnly => repitch(input, factor),
        StretchMode::Wsola => wsola(input, factor, tolerance.max(1)),
    })
}

/// Linear-interp resample reading `factor`× faster. `sample_rate` is
/// irrelevant: the engine plays the returned buffer at its own rate.
fn repitch(input: &[f32], factor: f32) -> Vec<f32> {
    let out_len = ((input.len() as f32) / factor) as usize;
    let mut out = vec![0.0f32; out_len];
    for (o, slot) in out.iter_mut().enumerate() {
        let pos = o as f32 * factor;
        let i = pos as usize;
        let b = input[(i + 1).min(input.len() - 1)];
        let frac = pos - i as f32;
        *slot = input[i] + (b - input[i]) * frac;
    }
    out
}

/// WSOLA overlap-add with similarity search. Output length is
/// `input.len() / factor` frames; pitch is preserved.
fn wsola(input: &[f32], factor: f32, tolerance: usize) -> Vec<f32> {
    let out_len = ((input.len() as f32) / factor) as usize;
    let mut out = vec![0.0f32; out_len];
    let mut norm = vec![0.0f32; out_len];
    if input.len() < 2 || out_len == 0 {
        return out;
    }
    let stretch_ratio = out_len as f32 / input.len() as f32;
    // Analysis (input) hop; fractional so very large/small factors stay exact.
    let analysis_hop = HOP as f32 / stretch_ratio.max(1e-6);
    let window: Vec<f32> = (0..FRAME)
        .map(|j| 0.5 * (1.0 - (std::f32::consts::TAU * j as f32 / FRAME as f32).cos()))
        .collect();

    // Position in input of the "natural continuation" the next frame should
    // sound like; unset until the first frame is placed.
    let mut reference: Option<usize> = None;
    let mut analysis_pos = 0.0f32;
    let mut out_pos = 0usize;
    while out_pos + FRAME <= out_len {
        let ideal = analysis_pos.round() as isize;
        let best = match reference {
            Some(ref_start) => {
                let mut best = ideal;
                let mut best_score = f32::NEG_INFINITY;
                for m in -(tolerance as isize)..=(tolerance as isize) {
                    let cand = ideal + m;
                    if cand < 0
                        || cand + HOP as isize > input.len() as isize
                        || ref_start + HOP > input.len()
                    {
                        continue;
                    }
                    let mut score = 0.0f32;
                    for k in 0..HOP {
                        score += input[ref_start + k] * input[(cand as usize) + k];
                    }
                    if score > best_score {
                        best_score = score;
                        best = cand;
                    }
                }
                best
            }
            None => ideal,
        };
        let best = best.max(0) as usize;
        for (j, w) in window.iter().enumerate() {
            let frame = out_pos + j;
            let src = best + j;
            if frame >= out_len || src >= input.len() {
                break;
            }
            out[frame] += input[src] * w;
            norm[frame] += w;
        }
        reference = Some((best + HOP).min(input.len().saturating_sub(HOP)));
        analysis_pos += analysis_hop;
        out_pos += HOP;
    }
    // Window-sum normalization keeps a constant input constant (Hann at 75%
    // overlap sums to exactly 2.0 everywhere).
    for (o, n) in out.iter_mut().zip(norm.iter()) {
        if *n > 1e-6 {
            *o /= *n;
        } else {
            *o = 0.0;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 48_000;

    fn sine(freq: f32, seconds: f32) -> Vec<f32> {
        let n = (seconds * SR as f32) as usize;
        (0..n)
            .map(|i| (std::f32::consts::TAU * freq * i as f32 / SR as f32).sin())
            .collect()
    }

    /// Zero crossings per second — a duration-independent pitch proxy.
    fn crossing_rate(x: &[f32], sr: u32) -> f32 {
        let crossings = x.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count() as f32;
        crossings / (x.len() as f32 / sr as f32)
    }

    #[test]
    fn factor_out_of_bounds_is_rejected() {
        let x = sine(220.0, 0.1);
        assert!(matches!(
            stretch(&x, SR, StretchMode::Wsola, 0.1),
            Err(RecipeError::OutOfBounds { field: "stretch factor", .. })
        ));
        assert!(stretch(&x, SR, StretchMode::RepitchOnly, 5.0).is_err());
        assert!(stretch(&x, SR, StretchMode::Wsola, 1.0).is_ok());
    }

    #[test]
    fn lengths_follow_the_factor() {
        let x = sine(220.0, 1.0);
        for mode in [StretchMode::RepitchOnly, StretchMode::Wsola] {
            for f in [0.5f32, 1.0, 2.0] {
                let out = stretch(&x, SR, mode, f).expect("stretch");
                let want = (x.len() as f32 / f) as usize;
                assert!(
                    (out.len() as i64 - want as i64).abs() <= HOP as i64 + 1,
                    "{mode:?} at {f}: {} vs {want}",
                    out.len()
                );
            }
        }
    }

    #[test]
    fn repitch_scales_pitch_with_the_factor() {
        let x = sine(220.0, 1.0);
        let up = stretch(&x, SR, StretchMode::RepitchOnly, 2.0).expect("stretch");
        let ratio = crossing_rate(&up, SR) / crossing_rate(&x, SR);
        assert!((ratio - 2.0).abs() < 0.05, "pitch ratio {ratio}");
    }

    #[test]
    fn wsola_preserves_pitch_while_conforming_tempo() {
        let x = sine(220.0, 1.0);
        let out = stretch(&x, SR, StretchMode::Wsola, 1.25).expect("stretch");
        let ratio = crossing_rate(&out, SR) / crossing_rate(&x, SR);
        assert!((ratio - 1.0).abs() < 0.05, "pitch drifted: ratio {ratio}");
        let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak > 0.5 && peak < 1.5, "amplitude ran away: {peak}");
    }

    #[test]
    fn wsola_keeps_constant_input_constant() {
        // Proves the window-square normalization: DC in, DC out at unity.
        let x = vec![0.8f32; SR as usize];
        let out = stretch(&x, SR, StretchMode::Wsola, 0.75).expect("stretch");
        let mid = &out[out.len() / 4..3 * out.len() / 4];
        assert!(mid.iter().all(|&s| (s - 0.8).abs() < 1e-3));
    }

    #[test]
    fn both_modes_are_bit_deterministic() {
        let x = sine(220.0, 0.5);
        for mode in [StretchMode::RepitchOnly, StretchMode::Wsola] {
            let a = stretch(&x, SR, mode, 1.3).expect("stretch");
            let b = stretch(&x, SR, mode, 1.3).expect("stretch");
            assert!(a.iter().zip(b.iter()).all(|(p, q)| p.to_bits() == q.to_bits()));
        }
    }

    #[test]
    fn degenerate_inputs_are_safe() {
        assert!(stretch(&[], SR, StretchMode::Wsola, 1.0).expect("empty").is_empty());
        let one = stretch(&[0.5], SR, StretchMode::Wsola, 1.0).expect("one");
        assert!(!one.is_empty());
    }
}
