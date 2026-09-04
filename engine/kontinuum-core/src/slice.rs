//! Sample slicing (issue #41 v1): transient detection over a PCM buffer.
//! Onset envelope -> adaptive peak picking with a minimum spacing, returning
//! frame offsets usable as slice starts. Pure, deterministic, allocation-free
//! after the output vec.

use std::sync::Arc;

/// Sorted sample-frame offsets bounding one-shot playback regions (issue
/// #19): slice k plays `table[k]..table[k+1]`, the last runs to the end of
/// the buffer. Built once from [`detect_slices`] and handed to the sampler
/// voices at attach time.
pub type SliceTable = Arc<[usize]>;

/// Detect slice start frames in a mono PCM buffer.
///
/// `max_slices` caps the result; `sensitivity` 0..1 scales the adaptive
/// threshold (higher = fewer slices). Always returns at least [0].
pub fn detect_slices(sample: &[f32], sr: u32, max_slices: usize, sensitivity: f32) -> Vec<usize> {
    if sample.len() < sr as usize / 20 || max_slices == 0 {
        return vec![0];
    }
    let env_hz = 200u32;
    let win = (sr / env_hz).max(1) as usize;
    let mut env: Vec<f32> = Vec::with_capacity(sample.len() / win + 1);
    let mut i = 0;
    while i + win <= sample.len() {
        env.push((sample[i..i + win].iter().map(|s| s * s).sum::<f32>() / win as f32).sqrt());
        i += win;
    }
    // Half-wave rectified difference = onset strength.
    let onset: Vec<f32> = std::iter::once(0.0)
        .chain(env.windows(2).map(|w| (w[1] - w[0]).max(0.0)))
        .collect();
    let mut sorted = onset.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = sorted[sorted.len() / 2];
    let threshold = median + (sorted[sorted.len() - 1] - median) * (1.0 - sensitivity.clamp(0.0, 1.0) * 0.8);

    let min_spacing = (env_hz as f32 * 0.06) as usize; // >= 60 ms between slices
    let mut slices = vec![0usize];
    let mut last_peak = 0usize;
    for (j, &o) in onset.iter().enumerate().skip(1) {
        if slices.len() >= max_slices {
            break;
        }
        if o > threshold && o >= onset[j.saturating_sub(1)] && o >= onset.get(j + 1).copied().unwrap_or(0.0)
            && j * win >= last_peak + min_spacing
        {
            slices.push(j * win);
            last_peak = j * win;
        }
    }
    slices
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_click_onsets() {
        // Two decaying clicks at 0.25 s and 0.75 s (48 kHz).
        let sr = 48_000u32;
        let mut sample = vec![0.0f32; sr as usize];
        for &pos in &[sr as usize / 4, sr as usize * 3 / 4] {
            for i in 0..2000 {
                sample[pos + i] += (-(i as f32) / 300.0).exp()
                    * (std::f32::consts::TAU * 200.0 * i as f32 / sr as f32).sin()
                    * 0.9;
            }
        }
        let slices = detect_slices(&sample, sr, 8, 0.5);
        assert_eq!(slices[0], 0);
        assert!(slices.len() >= 2, "both clicks found: {:?}", slices);
        assert!(slices[1] > sr as usize / 5, "second slice near first click: {:?}", slices);
    }

    #[test]
    fn silent_sample_returns_single_slice() {
        let sample = vec![0.0f32; 48_000];
        assert_eq!(detect_slices(&sample, 48_000, 8, 0.5), vec![0]);
    }

    #[test]
    fn max_slices_caps_result() {
        let sr = 48_000u32;
        let mut sample = vec![0.0f32; sr as usize];
        for k in 0..8 {
            let pos = k * sr as usize / 8;
            for i in 0..1000 {
                sample[pos + i] = 0.9;
            }
        }
        let slices = detect_slices(&sample, sr, 3, 0.5);
        assert!(slices.len() <= 3);
    }
}
