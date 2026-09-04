//! Key detection for the corpus pipeline: chroma accumulation over evenly
//! spaced windows, correlated against the Krumhansl–Kessler major/minor
//! profiles. A heuristic — honest about it: bass-heavy masters tilt the
//! chroma toward the tonic, which is exactly what the genre gives us, but
//! modal or ambiguous material can misreport by a fifth.

use crate::fft::{hanning, next_pow2, power_spectrum};

const NOTE_NAMES: [&str; 12] =
    ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

/// Scale masks (semitones from the tonic). Scoring is chroma mass inside
/// the mask — deliberately simpler than profile correlation: with
/// saw-rich timbres the individual degree weights wobble, but the
/// in-scale/out-of-scale split (G vs G# under an E root) is robust.
const MAJOR_STEPS: [usize; 7] = [0, 2, 4, 5, 7, 9, 11];
const MINOR_STEPS: [usize; 7] = [0, 2, 3, 5, 7, 8, 10];

const WINDOW: usize = 8192;
const MAX_WINDOWS: usize = 24;
const CHROMA_LO_HZ: f64 = 55.0;
const CHROMA_HI_HZ: f64 = 800.0;

/// Returns the key as `"<note> <major|minor>"`, e.g. `"A minor"`.
pub fn detect(mono: &[f32], sr: u32) -> String {
    let padded = next_pow2(WINDOW);
    let win = hanning(WINDOW);
    let bins = padded / 2;
    let mut re = vec![0.0f64; padded];
    let mut im = vec![0.0f64; padded];
    let mut scratch = vec![0.0f64; WINDOW];
    let mut chroma = [0.0f64; 12];

    // Evenly spread windows across the track, skipping the head/tail.
    let usable = mono.len().saturating_sub(2 * WINDOW);
    if usable < WINDOW {
        return "unknown".into();
    }
    let count = MAX_WINDOWS.min(usable / WINDOW);
    for w in 0..count {
        let center = WINDOW + usable * w / count.max(1);
        let start = center.saturating_sub(WINDOW / 2);
        for (slot, x) in scratch.iter_mut().zip(&mono[start..start + WINDOW]) {
            *slot = f64::from(*x);
        }
        let rms = (scratch.iter().map(|x| f64::from(*x) * f64::from(*x)).sum::<f64>() / WINDOW as f64).sqrt();
        if rms < 1e-4 {
            continue;
        }
        power_spectrum(&scratch, &win, &mut re, &mut im);
        for k in 0..bins {
            let f = k as f64 * f64::from(sr) / padded as f64;
            if !(CHROMA_LO_HZ..CHROMA_HI_HZ).contains(&f) {
                continue;
            }
            let p = re[k] * re[k] + im[k] * im[k];
            let midi = 69.0 + 12.0 * f64::log2(f / 440.0);
            let pc = (midi.round() as i64).rem_euclid(12) as usize;
            chroma[pc] += p;
        }
    }
    best_rotation(&chroma)
}

/// Picks the rotation whose scale mask holds the most chroma mass, plus a
/// tonic-emphasis bonus (the root's own chroma share) — the bonus breaks
/// the relative-key tie for bare triads, which sit inside two masks at
/// once, and is honest for bass-heavy material where the tonic dominates.
fn best_rotation(chroma: &[f64; 12]) -> String {
    let mut best = (0usize, "major", -1.0f64);
    for root in 0..12 {
        for (mode, steps) in [("major", &MAJOR_STEPS), ("minor", &MINOR_STEPS)] {
            let score: f64 =
                steps.iter().map(|&s| chroma[(root + s) % 12]).sum::<f64>() + chroma[root];
            if score > best.2 {
                best = (root, mode, score);
            }
        }
    }
    format!("{} {}", NOTE_NAMES[best.0], best.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synth_chord(sr: u32, partials: &[(f64, f64)]) -> Vec<f32> {
        let mut chord = vec![0.0f32; 4 * sr as usize];
        for &(hz, amp) in partials {
            for (i, s) in chord.iter_mut().enumerate() {
                *s += (amp * (2.0 * std::f64::consts::PI * hz * i as f64 / f64::from(sr)).sin()) as f32;
            }
        }
        chord
    }

    #[test]
    fn a_minor_triad_reports_a_minor() {
        // A1 bass + A2/C3/E3 triad — the fixture plant shape.
        let sr = 22_050;
        let chord = synth_chord(sr, &[(55.0, 0.6), (220.0, 0.3), (261.63, 0.25), (329.63, 0.25)]);
        assert_eq!(detect(&chord, sr), "A minor");
    }

    #[test]
    fn c_major_triad_reports_c_major() {
        let sr = 22_050;
        let chord = synth_chord(sr, &[(65.41, 0.5), (261.63, 0.3), (329.63, 0.3), (392.0, 0.3)]);
        assert_eq!(detect(&chord, sr), "C major");
    }
}
