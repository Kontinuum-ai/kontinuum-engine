//! Iterative radix-2 FFT, in-place, f64 precision. Dependency-free: the
//! critic must stay as deployable as the rest of the workspace, and power
//! spectra don't need a optimized library at analysis-time budgets.

/// Smallest power of two >= `n`.
pub fn next_pow2(n: usize) -> usize {
    n.next_power_of_two()
}

/// In-place iterative radix-2 Cooley–Tukey. `re`/`im` must have a power-of-two
/// length.
pub fn transform(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    debug_assert!(n.is_power_of_two());
    debug_assert_eq!(re.len(), im.len());
    if n <= 1 {
        return;
    }
    // Bit-reversal permutation.
    let bits = n.trailing_zeros();
    for i in 0..n {
        let j = (i.reverse_bits() >> (usize::BITS - bits)) as usize;
        if j > i {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    // Butterfly passes.
    let mut len = 2;
    while len <= n {
        let ang = -std::f64::consts::TAU / len as f64;
        let (wr_step, wi_step) = (ang.cos(), ang.sin());
        for start in (0..n).step_by(len) {
            let (mut wr, mut wi) = (1.0f64, 0.0f64);
            for k in 0..len / 2 {
                let a = start + k;
                let b = a + len / 2;
                let (xr, xi) = (re[b] * wr - im[b] * wi, re[b] * wi + im[b] * wr);
                re[b] = re[a] - xr;
                im[b] = im[a] - xi;
                re[a] += xr;
                im[a] += xi;
                let (nwr, nwi) = (wr * wr_step - wi * wi_step, wr * wi_step + wi * wr_step);
                wr = nwr;
                wi = nwi;
            }
        }
        len *= 2;
    }
}

/// Hann window of `n` points (symmetric, matching `numpy.hanning`).
pub fn hanning(n: usize) -> Vec<f64> {
    (0..n)
        .map(|i| {
            if n == 1 {
                1.0
            } else {
                0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / (n - 1) as f64).cos()
            }
        })
        .collect()
}

/// Power spectrum of a windowed real frame: `|FFT(x·w)|²` for rfft bins.
pub fn power_spectrum(frame: &[f64], window: &[f64], re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    for i in 0..n {
        re[i] = frame.get(i).copied().unwrap_or(0.0) * window.get(i).copied().unwrap_or(0.0);
        im[i] = 0.0;
    }
    transform(re, im);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64, eps: f64) -> bool {
        (a - b).abs() < eps
    }

    #[test]
    fn fft_recovers_sine_magnitudes() {
        let n = 1024;
        let freq = 16.0; // cycles in the window
        let mut re: Vec<f64> = (0..n)
            .map(|i| (std::f64::consts::TAU * freq * i as f64 / n as f64).sin())
            .collect();
        let mut im = vec![0.0; n];
        transform(&mut re, &mut im);
        let mag = |k: usize| (re[k] * re[k] + im[k] * im[k]).sqrt();
        // Peak at bin 16, ~zero at its neighbour's peak bin.
        assert!(approx(mag(16) / (n as f64 / 2.0), 1.0, 0.01), "bin16 {}", mag(16));
        assert!(mag(15) < mag(16) * 0.01 + 1e-9);
        assert!(mag(20) < mag(16) * 0.01 + 1e-9);
    }

    #[test]
    fn hanning_ends_near_zero_and_peaks_center() {
        let w = hanning(64);
        assert!(w[0].abs() < 1e-6);
        assert!(w[63].abs() < 1e-6);
        // Even-length symmetric Hann peaks just under 1.0 (numpy parity).
        let max = w.iter().cloned().fold(0.0, f64::max);
        assert!(max > 0.99, "hann peak {max}");
    }

    #[test]
    fn next_pow2_basics() {
        assert_eq!(next_pow2(1), 1);
        assert_eq!(next_pow2(1000), 1024);
    }
}
